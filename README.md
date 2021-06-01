# backroll-rs

[![crates.io](https://img.shields.io/crates/v/backroll.svg)](https://crates.io/crates/backroll)
[![Documentation](https://docs.rs/backroll/badge.svg)](https://docs.rs/backroll)
![License](https://img.shields.io/crates/l/backroll)
[![Discord](https://img.shields.io/discord/151219753434742784.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/VuZhs9V)

Backroll is a pure Rust implementation of [GGPO](https://www.ggpo.net/)
rollback networking library.

## Development Status
This is still in an untested alpha stage. At time of writing, the public facing API 
is largely stable, but a large portion of the functionality provided is still untested 
in real games.

## Differences with the C++ implementation

 * (Almost) 100% pure **safe** Rust. No unsafe pointer manipulation.
 * Type safety. backroll-rs heavily utilizes generics to allow for
 * Abstracted transport layer protocols - integrate and use any transport layer
   library you need. Comes with a raw UDP socket based implementation.
 * Configurable at compile time - Almost all Backroll types takes a
   `BackrollConfig` generic type parameter that specifies session constants.
   Does not require forking to change the hardcoded constants.
 * Reduced memory usage - Backroll's use of generics potentially shrinks down
   the sizes of many data types.
 * Vectorized input compression scheme - Backroll utilizes the same XOR + RLE
   encoding, but it's written to maximize CPU utilization.
 * Multithreaded I/O - All network communications run within an async task pool.
   I/O polling is no longer manual, nor blocks your game's execution.

## Repository Structure
This repo contains the following crates:

 * backroll - the main Backroll interface, intended to be used as the original
   GGPO. (Complete, untested)
 * backroll\_transport - An isolated set of transport layer abstractions. 
 * backroll\_transport\_udp - A transport layer implementation using raw UDP
   sockets. 
 * bevy\_backroll - a integration plugin for [bevy](https://bevyengine.org/).
   (In Progress, untested).