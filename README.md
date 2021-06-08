# backroll-rs

[![crates.io](https://img.shields.io/crates/v/backroll.svg)](https://crates.io/crates/backroll)
[![Documentation](https://docs.rs/backroll/badge.svg)](https://docs.rs/backroll)
![License](https://img.shields.io/crates/l/backroll)
[![Discord](https://img.shields.io/discord/151219753434742784.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/VuZhs9V)

Backroll is a pure Rust implementation of [GGPO](https://www.ggpo.net/)
rollback networking library.

## Development Status
This is still in an early beta stage. At time of writing, the public facing API 
is stable, and has undergone limited testing. There may still be notable bugs
that have not been found yet.

## Differences with the C++ implementation

 * (Almost) 100% pure **safe** Rust. No unsafe pointer manipulation.
 * Type safety. backroll-rs heavily utilizes generics and associated types to 
   avoid serialization overhead and potentially unsafe type conversions when 
   saving and loading game state.
 * Abstracted transport layer protocols - integrate and use any transport layer
   library you need. Comes with a raw UDP socket based implementation.
 * Configurable at runtime - Many of the hard-coded constants in GGPO are exposed
   as configuration parameters during session initialization.
 * Reduced memory usage - Backroll's use of generics potentially shrinks down
   the sizes of many data types.
 * Vectorized input compression scheme - Backroll utilizes the same XOR + RLE
   encoding, but it's written to maximize CPU utilization.
 * Multithreaded I/O - All network communications run within an async task pool.
   I/O polling is no longer manual, nor blocks your game's execution.

## Repository Structure
This repo contains the following crates:

 * backroll - the main Backroll interface, intended to be used as the original
   GGPO.
 * backroll\_transport - An isolated set of transport layer abstractions. 
 * backroll\_transport\_udp - A transport layer implementation using raw UDP
   sockets. 
 * backroll\_transport\_steam - A transport layer implementation using the 
   Steam provided networking utilities. Enables access to the Steam Datagram
   Relay service.
 * bevy\_backroll - a integration plugin for [bevy](https://bevyengine.org/).
   (Complete, untested).
 * lib - third-party linking dependencies (i.e. Steam) for easier local 
   development

# Building Locally
Due to linking requirements for backroll\_transport\_steam, local builds that 
include the crate currently only build when the Steamworks SDK is available.

According to steamworks-rs, at build time the `STEAM_SDK_LOCATION` env var must be 
set to a copy of the [Steamworks SDK](https://partner.steamgames.com/doc/sdk) to
properly link against.

The Backroll repo has an embedded copy of the Steamworks SDK saved within it.
If you are using Rust nightly v1.52+, cargo will automatically set this variable
and use the embedded copy of the Steamworks SDK. This is unfortunately unavailable
to stable Rust until [`configurable-env`](https://doc.rust-lang.org/nightly/cargo/reference/unstable.html#configurable-env)
stablizes.

If you are not using Steamworks, you can ignore this entirely. However, blanket
workspace builds (i.e. `cargo build` in the repo root) will include the crate by 
default. Using cargo dependencies via `path` should still work so long as Steam 
support is not needed.