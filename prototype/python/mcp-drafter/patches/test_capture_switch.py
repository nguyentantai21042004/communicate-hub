"""Tests for the runtime capture switch (TELEGRAM_CAPTURE_FILE)."""

import json
import os
from types import SimpleNamespace

import pytest

from telegram_mcp.tools import events


@pytest.fixture(autouse=True)
def _clean_state(monkeypatch, tmp_path):
    monkeypatch.setattr(events, "_pending_msgs", {})
    monkeypatch.setattr(events, "_feed_task", None)
    monkeypatch.setattr(events, "_activity_event", None)
    monkeypatch.setattr(events, "_feed_autostart_done", False)
    monkeypatch.setattr(events, "_capture_cache", (None, (True, None)))
    monkeypatch.delenv("TELEGRAM_EVENT_FEED", raising=False)
    monkeypatch.delenv("TELEGRAM_ALLOWED_CHAT_IDS", raising=False)
    monkeypatch.setenv("TELEGRAM_EVENT_FEED_FILE", str(tmp_path / "feed.jsonl"))
    monkeypatch.setenv("TELEGRAM_CAPTURE_FILE", str(tmp_path / "capture.json"))
    yield


def _write(data, bump=0):
    path = events.capture_file_path()
    path.write_text(data if isinstance(data, str) else json.dumps(data), encoding="utf-8")
    # Force a distinct mtime so the cache sees back-to-back rewrites.
    st = path.stat()
    os.utime(path, ns=(st.st_atime_ns, st.st_mtime_ns + bump * 1_000_000))


def _event(chat_id, username):
    sender = SimpleNamespace(
        bot=False, is_self=False, username=username, id=chat_id, first_name="X"
    )

    async def get_sender():
        return sender

    return SimpleNamespace(
        is_private=True, chat_id=chat_id, message=SimpleNamespace(id=1), get_sender=get_sender
    )


def test_missing_file_captures_everything():
    assert events.is_capture_wanted(42)


def test_off_blocks_all():
    _write({"enabled": False, "chats": []})
    assert not events.is_capture_wanted(42)


def test_chat_list_by_id_and_username():
    _write({"enabled": True, "chats": [42, "@Alice"]})
    assert events.is_capture_wanted(42)
    assert events.is_capture_wanted(7, SimpleNamespace(id=7, username="alice"))
    assert not events.is_capture_wanted(99, SimpleNamespace(id=99, username="bob"))


def test_rewrite_is_picked_up_without_restart():
    _write({"enabled": True})
    assert events.is_capture_wanted(42)
    _write({"enabled": False}, bump=5)
    assert not events.is_capture_wanted(42)


def test_broken_file_fails_closed():
    _write("{not json")
    assert not events.is_capture_wanted(42)


@pytest.mark.asyncio
async def test_handler_skips_unselected_threads():
    _write({"enabled": True, "chats": ["alice"]})
    await events._on_new_incoming(_event(1, "alice"))
    await events._on_new_incoming(_event(2, "bob"))
    assert set(events._pending_msgs) == {1}


@pytest.mark.asyncio
async def test_status_reports_capture_state():
    _write({"enabled": False})
    state = events.incoming_feed_state()
    assert state["capture_enabled"] is False
    assert state["capture_file"] == str(events.capture_file_path())
