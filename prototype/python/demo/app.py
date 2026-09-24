"""Telegram payload demo: watch the raw updates your account receives, and send as yourself.

Run (uses the session in ~/.config/telegram-captain/env):
  refs/chigwell__telegram-mcp/.venv/bin/python prototype/python/demo/app.py   (from the repo root)
Then open http://127.0.0.1:8766

Do not run this while bin/serve is up: two processes on one session get it revoked.
"""

import asyncio
import base64
import datetime
import io
import json
import os
import socket
from collections import deque
from contextlib import asynccontextmanager
from pathlib import Path

import uvicorn
from starlette.applications import Starlette
from starlette.requests import Request
from starlette.responses import HTMLResponse, JSONResponse, Response, StreamingResponse
from starlette.routing import Route
from telethon import TelegramClient, events, utils
from telethon.sessions import StringSession

PORT = 8766
MAX_UPLOAD = 20 * 1024 * 1024  # keep the demo from pushing huge files through one request
ENV_FILE = Path(os.getenv("CAPTAIN_ENV_FILE") or Path.home() / ".config" / "telegram-captain" / "env")
HTML = (Path(__file__).parent / "index.html").read_text(encoding="utf-8")

history: deque = deque(maxlen=300)  # replayed to a browser tab that connects late
subscribers: set[asyncio.Queue] = set()
client: TelegramClient


def load_env() -> dict:
    env = {}
    for line in ENV_FILE.read_text(encoding="utf-8").splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, _, value = line.partition("=")
            env[key.strip()] = value.strip()
    return env


def to_json(obj):
    """json.dumps fallback for what Telethon's to_dict() leaves in: datetimes and raw bytes."""
    if isinstance(obj, datetime.datetime):
        return obj.isoformat()
    if isinstance(obj, bytes):
        return "base64:" + base64.b64encode(obj).decode()
    return repr(obj)


def publish(item: dict) -> None:
    history.append(item)
    for q in subscribers:
        q.put_nowait(item)


async def summarize(update) -> dict:
    """Best-effort human line for message updates; the raw payload is kept untouched next to it."""
    msg = getattr(update, "message", None)
    if isinstance(msg, str):  # UpdateShortMessage: text sits directly on the update
        return {"is_message": True, "from_id": update.user_id, "out": update.out, "text": msg}
    if msg is None or not hasattr(msg, "message"):
        return {"is_message": False}
    return {**await describe(msg), "media": media_info(msg)}


def media_info(msg):
    """Media metadata plus a URL the UI can load; the payload only carries ids and file_reference."""
    try:
        if not getattr(msg, "media", None) or msg.file is None:
            return None
    except Exception:
        return None  # exotic media (polls, geo, dice) have no file view
    return {
        "kind": type(msg.media).__name__,
        "mime": msg.file.mime_type,
        "name": msg.file.name,
        "size": msg.file.size,
        "url": f"/api/media/{utils.get_peer_id(msg.peer_id)}/{msg.id}",
    }


async def describe(msg) -> dict:
    name = None
    try:
        sender = await client.get_entity(msg.from_id or msg.peer_id)
        name = utils.get_display_name(sender)
    except Exception:
        pass  # entity not cached yet; the id is still in the payload
    return {"is_message": True, "from": name, "out": msg.out, "text": msg.message}


async def media(request: Request):
    peer, msg_id = int(request.path_params["peer"]), int(request.path_params["msg_id"])
    msg = await client.get_messages(peer, ids=msg_id)
    if msg is None or not msg.media:
        return JSONResponse({"error": "message has no media"}, status_code=404)
    data = await client.download_media(msg, file=bytes)
    if data is None:
        return JSONResponse({"error": "media not downloadable (e.g. web preview)"}, status_code=404)
    mime = msg.file.mime_type if msg.file else None
    # The feed re-renders on every update; let the browser keep the bytes instead of re-downloading.
    return Response(data, media_type=mime or "application/octet-stream", headers={"Cache-Control": "private, max-age=3600"})


async def on_raw(update) -> None:
    publish({
        "kind": "update",
        "received_at": datetime.datetime.now().isoformat(timespec="seconds"),
        "type": type(update).__name__,
        "summary": await summarize(update),
        "payload": update.to_dict(),
    })


def port_busy(port: int) -> bool:
    with socket.socket() as s:
        return s.connect_ex(("127.0.0.1", port)) == 0


@asynccontextmanager
async def lifespan(app):
    global client
    env = load_env()
    client = TelegramClient(
        StringSession(env["TELEGRAM_SESSION_STRING"]),
        int(env["TELEGRAM_API_ID"]),
        env["TELEGRAM_API_HASH"],
        device_model="Captain payload demo",
        system_version="macOS",
        app_version="0.1",
    )
    await client.connect()
    if not await client.is_user_authorized():
        raise SystemExit("session is not authorized; rerun session_string_generator.py --qr")
    client.add_event_handler(on_raw, events.Raw)
    await client.get_dialogs(limit=50)  # fills the entity cache so numeric chat ids resolve
    yield
    await client.disconnect()


async def index(request: Request):
    return HTMLResponse(HTML)


async def me(request: Request):
    u = await client.get_me()
    return JSONResponse({"id": u.id, "name": utils.get_display_name(u), "username": u.username})


async def dialogs(request: Request):
    out = []
    async for d in client.iter_dialogs(limit=40):
        ent = d.entity
        out.append({
            "id": d.id,
            "name": d.name,
            "username": getattr(ent, "username", None),
            "kind": "user" if d.is_user else "group" if d.is_group else "channel",
            "unread": d.unread_count,
        })
    return JSONResponse(out)


async def send(request: Request):
    form = await request.form(max_part_size=MAX_UPLOAD)
    chat, text = str(form.get("chat", "")).strip(), str(form.get("text", ""))
    upload = form.get("file")
    has_file = upload is not None and getattr(upload, "filename", "")
    if not chat or not (text.strip() or has_file):
        return JSONResponse({"ok": False, "error": "chat and text or file are required"}, status_code=400)
    target = int(chat) if chat.lstrip("-").isdigit() else chat
    try:
        if has_file:
            buf = io.BytesIO(await upload.read())
            buf.name = upload.filename  # Telethon picks photo vs document from the extension
            msg = await client.send_file(target, buf, caption=text or None)
        else:
            msg = await client.send_message(target, text)
    except Exception as e:
        return JSONResponse({"ok": False, "error": f"{type(e).__name__}: {e}"}, status_code=400)
    payload = json.loads(json.dumps(msg.to_dict(), default=to_json))
    publish({
        "kind": "sent",
        "received_at": datetime.datetime.now().isoformat(timespec="seconds"),
        "type": "Message (sent by this demo)",
        "summary": {"is_message": True, "from": "you", "out": True, "text": text, "media": media_info(msg)},
        "payload": payload,
    })
    return JSONResponse({"ok": True, "payload": payload})


async def stream(request: Request):
    q: asyncio.Queue = asyncio.Queue()
    subscribers.add(q)

    async def gen():
        try:
            for item in list(history):
                yield f"data: {json.dumps(item, default=to_json, ensure_ascii=False)}\n\n"
            while True:
                try:
                    item = await asyncio.wait_for(q.get(), timeout=15)
                    yield f"data: {json.dumps(item, default=to_json, ensure_ascii=False)}\n\n"
                except asyncio.TimeoutError:
                    yield ": keepalive\n\n"
        finally:
            subscribers.discard(q)

    return StreamingResponse(gen(), media_type="text/event-stream")


app = Starlette(
    routes=[
        Route("/", index),
        Route("/api/me", me),
        Route("/api/dialogs", dialogs),
        Route("/api/send", send, methods=["POST"]),
        Route("/api/media/{peer}/{msg_id}", media),
        Route("/api/stream", stream),
    ],
    lifespan=lifespan,
)

if __name__ == "__main__":
    if port_busy(8765):
        raise SystemExit("bin/serve is running on :8765 with the same session; stop it first")
    if port_busy(PORT):
        raise SystemExit(f"port {PORT} is taken; the demo is probably already running")
    uvicorn.run(app, host="127.0.0.1", port=PORT, log_level="warning")
