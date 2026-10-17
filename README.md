# ronf

A minimal, terminal-based multi-server IRC client written in Rust.

`ronf` connects to multiple IRC networks concurrently and presents a clean
interface designed to match a lightweight, keyboard-driven workflow.

## Installation & Build

Ensure you have a Rust toolchain installed:

```bash
cargo build --release
```

The resulting binary will be located at `target/release/ronf`.

## Configuration

`ronf` looks for its configuration file in the following order:

1. Path passed via command-line argument: `ronf /path/to/config.toml`
2. `~/.config/ronf/config.toml` (or `$XDG_CONFIG_HOME/ronf/config.toml`)
3. `./config.toml` in the current working directory

### Example `config.toml`

```toml
default_nickname = "irc_user"

[servers.libera]
server = "irc.libera.chat"
port = 6697
use_tls = true
channels = [
    "#linux",
    "##rust",
]

[servers.oftc]
server = "irc.oftc.net"
port = 6697
use_tls = true
channels = [
    "#kernelnewbies",
]
```

## Keybindings & Commands

### Tree & Buffer Navigation

- `Ctrl+n` / `Alt+Down`: Select next buffer in tree
- `Ctrl+p` / `Alt+Up`: Select previous buffer in tree
- `Alt+a`: Jump directly to next unread buffer (`*`)
- `Alt+1` .. `Alt+9`: Jump directly to buffer 1..9

### Scrolling

- `PgUp` / `PgDn`: Scroll chat history up / down
- `Alt+PgUp` / `Alt+PgDn`: Scroll right-hand Users list up / down
- `Esc`: Snap chat scrollback back to latest messages

### Input Editing (Readline)

- `Tab`: Auto-complete nickname or `/command` (press repeatedly to cycle)
- `Up` / `Down`: Navigate sent message history
- `Ctrl+w`: Delete word before cursor
- `Ctrl+u`: Clear input line
- `Home` (`Ctrl+a`) / `End` (`Ctrl+e`): Move cursor to start / end of line

### Slash Commands

- `/help`: Open the scrollable help modal
- `/join <#channel>`: Join channel on active server
- `/part [reason]`: Leave current channel
- `/msg <target> <text>`: Send private message or message channel
- `/nick <newnick>`: Change nickname on active server
- `/me <action>`: Send CTCP action message
- `/close`: Close current channel or direct message buffer
- `/quit [reason]`: Disconnect from all servers and exit

## License

This project is licensed under the GNU General Public License v3.0 or later
(GPL-3.0-or-later). See [LICENSE](LICENSE) for details.
