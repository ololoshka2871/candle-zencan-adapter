# candle-zencan-adapter

This crate provides a Rust adapter that implements the [zencan](https://github.com/mcbridejc/zencan) traits for Candle-compatible USB <-> CAN adapters.
The goal is to integrate Candle devices into the `zencan` ecosystem with an async-first design (Tokio).

## Credits

- Original Windows Candle API (C implementation) by Hubert Denkmair.
- [Candle.NET](https://github.com/elliotwoods/Candle.NET) (C# wrapper) by elliotwoods and contributors.

## Scope

- Provide a `zencan`-compatible adapter over Candle devices on Windows.
- Offer an async API surface suitable for Tokio-based applications.
- Linux uses the socketcan backend (outside this crate); Candle adapters are handled via socketcan.
- Non-Windows builds are intentionally disabled (build.rs enforces Windows-only support).

## Status

- Windows-only adapter crate; Linux support is delegated to socketcan.
