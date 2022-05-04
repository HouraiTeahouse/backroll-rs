# bevy-backroll

[![crates.io](https://img.shields.io/crates/v/bevy-backroll.svg)](https://crates.io/crates/bevy-backroll)
[![Documentation](https://docs.rs/bevy-backroll/badge.svg)](https://docs.rs/bevy-backroll)
![License](https://img.shields.io/crates/l/bevy-backroll)
[![Discord](https://img.shields.io/discord/151219753434742784.svg?label=&logo=discord&logoColor=ffffff&color=7389D8&labelColor=6A7EC2)](https://discord.gg/VuZhs9V)
[![Bevy tracking](https://img.shields.io/badge/Bevy%20tracking-released%20version-lightblue)](https://github.com/bevyengine/bevy/blob/main/docs/plugins_guidelines.md#main-branch-tracking)

A [Bevy](https://bevyengine.com) engine integration plugin for [backroll](https://crates.io/crates/backroll)
rollback networking library.

## Bevy Version Supported

|Bevy Version|bevy\_backroll|
|:-----------|:-------------|
|0.7         |0.4           |
|0.6         |0.2, 0.3      |
|0.5         |0.1           |

## Feature Flags

 - `steam` - Enables Steamworks SDK support, adds the `SteamP2PManager` from
   [backroll-transport-steam](https://crates.io/crates/bevy-backroll) as a
   resource to the app.