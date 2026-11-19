# Terminal chat

Aurora Chat is a separate, experimental terminal executable for **Local and
group text chat only**. It shares the viewer's login, TLS, packet codecs and
reliable UDP transport, but does not start or build a renderer, graphical
window, asset decoder, inventory, audio, voice or media subsystem.
The normal Aurora Viewer remains available.

## Build and run

From the task worktree, using the repository's pinned Rust toolchain:

```bash
cargo build -p aurora-chat --locked
cargo run -p aurora-chat --locked -- --demo
```

`--demo` is completely offline. Try `/groups`, `/join 1`, typing a message,
Tab, `/local`, PageUp and `/quit`. Demo messages are local echoes, not proof
that a live simulator accepted a message.

To connect, **run this yourself** in an interactive terminal:

```bash
cargo run -p aurora-chat --locked
```

Enter your account name, password and optional MFA code. Tab changes fields,
Enter connects, and F2 switches the start location between Last (default) and
Home. Passwords and MFA codes are masked. No credentials are accepted in
arguments, environment variables or files. No viewer accounts/settings/cache
are loaded, and neither credentials nor transcripts are saved. The client
uses per-process opaque device identifiers rather than hardware identifiers.
Terms and critical messages require an explicit Y/N decision after displaying
the server's text; then re-enter the password to retry. New-server login
redirects requiring consent are refused, not silently followed.

Options:

| Argument | Meaning |
|---|---|
| `--demo` | Offline simulation; never contacts a grid |
| `--language=en`, `--language=fr`, `--language=ja` | UI language; otherwise follows the system |
| `--help`, `-h` | Usage without starting the terminal UI |
| `--version`, `-V` | Version without starting the terminal UI |

Build the package specifically (`-p aurora-chat`), **not `--workspace`**, to
keep the dependency graph lean. To install just this executable:

```bash
cargo install --path crates/aurora-chat --locked --debug
```

## Chat commands and keys

| Command/key | Action |
|---|---|
| `/groups` | List the groups you already belong to |
| `/join <number or full name>` | Open one of those groups' chat sessions |
| `/part` | Close the current chat session; does **not** leave the group |
| `/local` | Switch to Local |
| Tab / Shift-Tab, `/next`, `/prev` | Change chat buffer, retaining its draft |
| `/whisper <text>`, `/shout <text>` | Whisper/shout in Local |
| `/me <action>` | Send an action in the current chat |
| `//text` | Send a literal leading slash |
| PageUp / PageDown | Scroll the current transcript |
| Left / Right / Home / End / Backspace / Delete | Edit UTF-8 input |
| Ctrl-U | Clear input |
| `/help` | Command reference |
| `/quit`, Ctrl-C or Esc | Request logout, wait for confirmation, restore terminal |

Wide terminals show a chat sidebar with unread counts. Narrow terminals show
only the active chat. Incoming group invitations are accepted only for groups
in the account's membership list, not arbitrary conferences or voice calls.
Group sends wait until session start is confirmed; text moderation is observed.
Own group messages appear after the simulator acknowledges the UDP packet:
that confirms transport, **not delivery to every participant**. Local messages
are displayed when echoed by the simulator. Failed acknowledgments and lost
event queues are reported explicitly rather than pretending delivery succeeded.

## Resource model and intentional limits

- One Tokio runtime worker, event-driven terminal redraw, no frame loop or GPU.
- A chat-only build bundles only terminal and shared login/session
  translations, not the graphical viewer's complete catalogs.
- At most 64 group-chat buffers plus Local; 128 rows per buffer and at most
  2048 UTF-8 bytes per displayed row. History is session-only and older rows
  are evicted. Input is capped at 4096 bytes; wire messages reuse the viewer's
  Unicode-safe 1023-byte splitting.
- 64 queued user commands, 128 UI events and 128 event-queue events. If the UI
  cannot keep up, the session stops with an explicit error instead of growing
  memory without bound. At most 128 reliable sends are pending.
- Only the login region is connected. The client requests zero Land, Wind,
  Cloud, Texture and Asset bandwidth, 8 kbit/s Resend and 16 kbit/s Task, and
  zero draw distance. Task must remain enabled for chat/control messages.
  The simulator may still send world packets; they are acknowledged and
  discarded **without decoding or retaining world objects**.
- No textures, meshes, inventory, appearance/outfit requests or cache files.
  Your avatar remains at the login location; its appearance is not managed.
- No movement, teleport/crossing support, direct IM UI, offline IM retrieval,
  group administration, mute-list management, chat persistence, saved
  accounts, updater or media. Unsupported offers are not automatically accepted.
- Standard HTTP proxy environment settings apply through the shared proxy
  helper; viewer-configured SOCKS UDP proxies are not loaded by this client.
- Closing the terminal unexpectedly cannot guarantee a server-confirmed
  logout; a live session may take time to expire. During a login request,
  quitting may wait for that request to finish before sending logout.

The aim is IRC-like resource use, not a claim that the Second Life protocol
has IRC's exact bandwidth or memory requirements. In particular, live-region
traffic and TLS/HTTP overhead need real measurements after human validation.

### Offline measurements

On Linux, the default development build after opening Local and a demo group
used approximately **6.5 MiB resident memory**, with two OS threads (the
current-thread runtime and terminal input helper), no IP sockets and no
measurable CPU ticks during a 30-second idle sample. Before excluding the graphical
translation catalogs it used about 48 MiB. These are offline process
measurements, not a live-session memory or bandwidth guarantee.

## Validation

Automated tests cover UTF-8 input and bounds, terminal-control sanitization,
masked login, tiny terminal sizes, wrapping, unread/draft state, commands,
membership/session gating, wire messages and chat-only throttles. Shared
login, TLS, reliable-circuit and event-queue tests remain applicable.

The standalone build, strict chat/default-viewer-network clippy checks,
207 selected workspace tests and 11 full-catalog integration tests pass.
A pseudo-terminal check verifies actual Local/group output, switching,
bracketed paste, resizing, quitting and restoration of terminal modes.
Full `scripts/check.ps1` validation is currently blocked in this Linux
environment by the graphical viewer's missing ALSA development library;
this is not a dependency of the standalone chat client.

The following checks **must be performed by a human**, not an automated agent,
before treating live chat as validated:

1. Log in with Last and Home; test MFA and, when offered, explicit terms consent.
2. With a second client nearby, exchange Unicode Local messages, whispers,
   shouts, `/me` actions and a message longer than 1023 bytes.
3. Use `/groups` and `/join`; exchange messages in multiple groups. Confirm
   incoming invitations, unread counts, moderation, `/part` and reopening.
4. Confirm `/part` does not remove group membership; ignored IM/asset offers
   are not accepted; no avatar movement or unexpected asset requests occur.
5. Leave the client idle for at least 30 minutes. Test disconnection and
   relogin, `/quit`, terminal restoration and immediate login from another client.
6. Measure resident memory, idle CPU and network traffic in quiet and busy
   regions. Compare with an IRC client and the graphical viewer on the same
   machine. Offline-demo numbers alone are not representative of live traffic.
