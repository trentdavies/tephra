# tephra

Layered memory for humans and their agents.

tephra turns any cloud-synced notes folder (Obsidian Sync, Syncthing, iCloud,
Dropbox, …) into versioned, auto-merged, agent-writable memory. You keep
editing notes in your apps; coding agents read and write the same notes
through git; a small daemon merges the two streams continuously — every
change versioned, conflicts never blocking and never losing data.

```
 phone / laptop apps ──── sync service ────┐
                                           ▼
                                    bridge checkout   ← tephra bridge (daemon)
                                           ▲
                                           │ push / fetch
                                    git remote (authoritative)
                                           ▲
                                           │ clone / pull / push
                                    agent clones (tephra clone / sync)
```

**Status: pre-0.1, under active development.** Design: [docs/DESIGN.md](docs/DESIGN.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
