# Terminal chat

Aurora Chat is a separate, experimental terminal executable for **Local,
group and direct text chat**. It shares the viewer's login, TLS, packet codecs and
reliable UDP transport, but does not start or build a renderer, graphical
window, asset decoder, inventory, audio, voice or media subsystem.
The normal Aurora Viewer remains available.

## Build and run

From the task worktree, using the repository's pinned Rust toolchain:

```bash
cargo build -p aurora-chat --locked
cargo run -p aurora-chat --locked -- --demo
```

`--demo` is completely offline. Try `/friends`, `/query 1`, `/radar`, `/local`,
`/3 roll`, `/groups`, `/join 1`, typing a message, Tab, PageUp and `/quit`.
The demo includes online/offline friends, an incoming DM and nearby residents
with known and unknown distances. Demo messages are local echoes, not proof
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
| `/friends [page]` | List friends and online/offline presence; 100 per page |
| `/query <recipient>` | Open a direct-message buffer |
| `/msg <recipient> <text>`, `/im <recipient> <text>` | Open a direct chat and send |
| `/radar [page]`, `/nearby [page]` | List nearby residents and approximate chat range; 100 per page |
| `/part` | Close the current group or direct chat; does **not** leave the group |
| `/local` | Switch to Local |
| Tab / Shift-Tab, `/next`, `/prev` | Change chat buffer, retaining its draft |
| `/whisper <text>`, `/shout <text>` | Whisper/shout in Local |
| `/3 roll`, `/3roll`, `/-3roll` | Send to that signed script channel, from Local only |
| `/me <action>` | Send an action in the current chat |
| `//text` | Send a literal leading slash |
| PageUp / PageDown | Scroll the current transcript |
| Left / Right / Home / End / Backspace / Delete | Edit UTF-8 input |
| Ctrl-U | Clear input |
| `/help` | Command reference |
| `/diagnostics` | Show a sanitized, in-memory protocol snapshot; never sends chat |
| `/quit`, Ctrl-C or Esc | Request logout, wait for confirmation, restore terminal |

Recipients can be a UUID, a 1-based number from `/friends`, or an exact full
display or legacy name, without quotes. Names are matched case-insensitively
for ASCII; ambiguous names are refused rather than guessing. For example,
`/msg Sakura Example hello` resolves the longest matching full-name prefix.
Radar numbers are not recipient shortcuts. Open a buffer first with `/query`
to avoid ambiguity between a name and the beginning of a message. `/friends`
numbers follow the current list; check it again after friendship changes.
Presence honors the grid's notifications: "offline" can also mean the friend
has hidden their online status from you.

Numeric channels are signed 32-bit values, excluding the reserved debug
channel `2147483647`. They never route to a group or direct chat. A nonzero
channel send shows a notice after the simulator acknowledges the packet;
it is not a public Local echo or confirmation that a HUD script ran.
Unlike the Legacy Viewer's repeat-channel shortcut, `//text` deliberately
keeps this client's original meaning: send literal `/text` to the active chat.

Messages and notices use receipt time from the machine's clock in Second Life
Time (SLT, `America/Los_Angeles`, including daylight-saving changes),
regardless of its local timezone. Messages use
`[YYYY/MM/DD HH:MM] Display (Legacy): text`. `/me` is retained literally, as
in SL chat logs. Identical names are shown once; legacy names or UUIDs remain usable
until the People API responds. Late lookups also resolve retained history,
DM titles, friend lists and radar. Scripted objects keep their object names.
This is in-memory formatting, not disk logging.

Wide terminals show a chat sidebar with unread counts. At 110 columns or more,
Local also shows a live radar sidebar. Narrow terminals show only the active
chat; `/radar` remains available. Radar uses the simulator's coarse map
locations, not object/appearance downloads: XY is quantized to roughly one
metre and Z to four metres on a standard region. Distances and the normal-chat
20-metre marker are **approximate**, especially near the boundary. Altitudes
at or above the coarse limit (1020 m) are unknown, not falsely "in range."
Only residents reported in the login region are listed; this is not proof
of who can hear you across a boundary or with unusual region chat settings.
Snapshots expire after 30 seconds without an update.

Incoming DMs open a separate buffer without taking focus; plain text in that
buffer replies to its peer. Direct messages do not require group session
startup. Queued offline IMs are requested after connecting, and offline friends
receive the protocol's offline delivery flag. Own DM echoes, like group echoes,
wait for the simulator's transport acknowledgment; it does not prove that the
recipient read or received the text.

Incoming group invitations are accepted only for groups
in the account's membership list, not arbitrary conferences or voice calls.
Group sends wait until session start is confirmed; text moderation is observed.
Own group messages appear after the simulator acknowledges the UDP packet:
that confirms transport, **not delivery to every participant**. Local messages
are displayed when echoed by the simulator. Failed acknowledgments and lost
event queues are reported explicitly rather than pretending delivery succeeded.

Object instant messages (`llInstantMessage`, protocol dialog 19) and script
alerts appear in Local under the object's name, including queued offline object
IMs. They do not open a direct chat to the object's owner or overwrite the
owner's resident name.

### Troubleshooting worn HUDs and animations

Scripts execute on the simulator, not in this terminal. A missing HUD response
can mean that the client did not display a message, not that the script stopped.
The client now displays object IMs as well as ordinary object chat. However,
script dialogs/text boxes and permission prompts are **not implemented**:
the first of each received in a session produces a notice, and no response or
permission grant is sent. No HUD touches, keyboard controls, local animation
playback, AO engine or animation-completion signaling are implemented either.
An AO or HUD that needs those viewer features may still fail.

Use `/diagnostics` for a targeted report during **your own** login:

1. Switch to `/local`, run `/diagnostics`, and retain the counter lines.
2. Try `/3 roll` once, wait a few seconds, then run `/diagnostics` again.
3. If applicable, have another participant trigger one incoming HUD event and
   take another snapshot.
4. Copy just the lines from `aurora_chat=... diagnostics=1` through the final
   `received_local_chat_types:` line, not the surrounding conversation. Widen
   the terminal or use PageUp to reach earlier rows.

The report contains numeric counters and fixed protocol/state labels, never
account/session IDs, credentials, names, chat/dialog contents, script or
animation asset IDs, region names, positions, simulator addresses or capability
URLs. It is generated in memory on demand, sends nothing to the grid and
writes no file. Counters reset on reconnect. Labels stay in a stable,
machine-readable form in every UI language.

- `script_channel_packets_sent` counts nonzero-channel UDP messages handed to
  the reliable transport; `acknowledged` counts simulator acknowledgments.
  Neither proves a script heard or executed the command.
- `received_im_dialogs: 19=...` counts object IMs, `31=...` object alerts,
  `0=...` resident IMs and `17=...` group messages.
- `received_local_chat_types` counts `ChatFromSimulator` packets by type,
  including OwnerSay (`8`). `local_chat_filtered` counts inaudible/typing
  messages excluded from the transcript.
- `script_questions` and `permission_bits_seen` identify unanswered permission
  requests without identifying their scripts. `script_dialogs` counts
  unanswered dialog/text-box requests.
- `self_animation_packets`, `latest_animations`, `latest_sources` and
  `last_update_age_seconds` describe the latest animation state received for
  your avatar only. `self_transition_states` names only built-in `standup`,
  `prejump`, `land` and `medium_land` states that ordinarily wait for viewer
  animation completion; it never exposes arbitrary asset IDs. No animation
  packet yet is different from a received packet with zero animations.

This is preferable to sharing a full viewer log or packet capture. In the
graphical Aurora Viewer, use Dev Toolbox **J7** (Script errors) to check script
errors yourself; share only an intentionally selected, non-private error if
needed. J7 does not reveal what the terminal received, so start with the
terminal's report for this problem.

## Resource model and intentional limits

- One Tokio runtime worker, event-driven terminal redraw, no frame loop or GPU.
- A chat-only build bundles only terminal and shared login/session
  translations, not the graphical viewer's complete catalogs.
- At most 64 group/direct-chat buffers plus Local; 128 rows per buffer and at most
  2048 UTF-8 bytes per displayed row. History is session-only and older rows
  are evicted. Input is capped at 4096 bytes; wire messages reuse the viewer's
  Unicode-safe 1023-byte splitting.
- At most 4096 friends, nearby entries and cached resident names. Name lookups
  run one batch of up to 64 at a time, with a ten-minute refresh interval and
  legacy UDP fallback; name responses are capped at 1 MiB. Old cached names
  are evicted rather than retaining every resident ever encountered.
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
- Diagnostics retain only bounded counters and known built-in transition
  labels. Self-animation and script-control packets are decoded only for these
  counters, without fetching or playing animations.
- No movement, teleport/crossing support, friendship offer/rights management,
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

After adding channels, friends, DMs, radar, display names, SLT timestamps and HUD
diagnostics, a Linux pseudo-terminal demo run with Local, group and direct chat used
**9024 KiB (about 8.8 MiB) resident memory**, two OS threads, no IP sockets and
zero CPU ticks in a 30-second idle sample. The demo remains entirely offline.

## Validation

Automated tests cover UTF-8 input and bounds, terminal-control sanitization,
masked login, tiny terminal sizes, wrapping, unread/draft state, commands,
membership/session gating, channel/DM wire fields, friend presence, late name
resolution, bounded People API responses, radar scaling/expiry, object IMs,
script-request visibility, diagnostic privacy/bounds and chat-only throttles.
Shared login, TLS, reliable-circuit and event-queue tests remain applicable.

The standalone build, strict lean-workspace and combined viewer/chat-network
clippy checks, 242 selected workspace tests and nine full-catalog integration
tests pass. Timestamp tests include winter, summer and both daylight-saving
transitions; a pseudo-terminal test confirmed SLT output with UTC, Tokyo and
Los Angeles host timezones. English, French and Japanese terminal checks confirm
object-message display and `/diagnostics` in Local and direct buffers. The HUD
follow-up changes no dependencies or licence notices.
A pseudo-terminal check verifies actual Local/group/direct output, channel
isolation, friends, radar, full-name recipients, switching, bracketed paste,
resizing, quitting and restoration of terminal modes.
Full `scripts/check.ps1` validation is currently blocked in this Linux
environment by the graphical viewer's missing ALSA development library;
this is not a dependency of the standalone chat client.

The following checks **must be performed by a human**, not an automated agent,
before treating all live chat features as validated. The human confirmed the
initial live login, Local and group chat on 2026-11-19; the additions below
still need fresh live validation:

1. Log in with Last and Home; test MFA and, when offered, explicit terms consent.
2. With a second client nearby, exchange Unicode Local messages, whispers,
   shouts, `/me` actions and a message longer than 1023 bytes.
3. Use `/groups` and `/join`; exchange messages in multiple groups. Confirm
   incoming invitations, unread counts, moderation, `/part` and reopening.
4. With the command-listening HUD already worn, use `/local`, then `/3 roll`;
   confirm the actual roll, signed channels where applicable, and that trying
   `/3 roll` from a group or DM is refused without sending there.
5. Compare `/radar` and the wide Local sidebar with a nearby client at normal
   chat distance and beyond it. Check departure updates and unknown altitude
   above 1020 m; allow for coarse-location error near 20 m.
6. Check `/friends` against known online/offline friends. Exchange incoming and
   outgoing DMs, including an offline friend; test reply drafts, unread counts,
   `/part`, and a queued IM delivered at login.
7. Check Local/group/DM senders with display names different from legacy names,
   including Unicode, your own name and a scripted object. Confirm SLT
   timestamps and `[timestamp] Display (Legacy): /me ...` formatting, including
   when the machine is set to a different timezone.
8. Confirm `/part` does not remove group membership; ignored IM/asset offers
   are not accepted; no avatar movement or unexpected asset requests occur.
9. Leave the client idle for at least 30 minutes. Test disconnection and
   relogin, `/quit`, terminal restoration and immediate login from another client.
10. Measure resident memory, idle CPU and network traffic in quiet and busy
   regions. Compare with an IRC client and the graphical viewer on the same
   machine. Offline-demo numbers alone are not representative of live traffic.
