# kley

Run chat with `./docker-session.sh`; run self-improve with `./docker-session.sh self-improve.sh`.

## CLI quick reference

```bash
# Start interactive chat
cargo run -- chat

# Start interactive chat and allow tools without prompts (legacy)
cargo run -- chat --yolo

# Start interactive chat and allow tools without prompts (new)
cargo run -- chat --tool-approval auto

# Start interactive chat but deny all tool calls
cargo run -- chat --tool-approval never

# Run in autonomous mode (must provide an initial prompt)
cargo run -- chat --autonomous --prompt "Improve repo ergonomics"

# Use a lower context compaction threshold
cargo run -- chat --compact-threshold 200000
```
