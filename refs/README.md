# Reference repositories

Read-only clones used to check APIs and payload shapes. Git ignores everything here except this
file. Recreate a clone with `git clone <url> refs/<dir>` and check out the commit below.

| Dir | Upstream | Commit | Why it is here |
|---|---|---|---|
| `grammers` | <https://codeberg.org/Lonami/grammers.git> | `0783c28` | The MTProto library `captain-protocol` uses. |
| `Telethon` | <https://codeberg.org/Lonami/Telethon.git> | `7e0bf14` | Python MTProto client behind the prototype demo. |
| `FedericoBruzzone__tdlib-rs` | <https://github.com/FedericoBruzzone/tdlib-rs.git> | `3bfef94` | TDLib bindings, the alternative adapter that was not chosen. |
| `chigwell__telegram-mcp` | <https://github.com/chigwell/telegram-mcp> | `45cce7e` | Telethon MCP server; its `.venv` runs the Python demo. |
| `chaindead__telegram-mcp` | <https://github.com/chaindead/telegram-mcp> | `52d7991` | Go MCP server, compared during research. |

The crates.io release grammers 0.10.0 is what the workspace builds against. The clone is newer
and may differ.
