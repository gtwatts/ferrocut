# Cutline

Headless, agent-native video editing and compositing engine (Rust).

Brief: Obsidian vault `Pi Memory/Projects/cutline-research-brief-2026-10-07.md`.

## Crate ownership
- `cutline-core`: shared frame type, render node trait, rational time (Rusty + SeePlus)
- `cutline-engine`: timeline, scheduler, render graph, FFmpeg I/O, wgpu compositor (Rusty)
- `cutline-color`: OCIO bridge (SeePlus)
- `cutline-ofx`: out-of-process OpenFX host (SeePlus)
