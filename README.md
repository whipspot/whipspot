# Whipspot

A desktop music player written in Rust. Spotify, YouTube Music and your own files, in one player that cares about sound quality.

![Whipspot's library view](.github/readme/library.jpg)

> **Early pre-release.** Built for Windows 11. The macOS build is experimental.

## What it does

| | |
|---|---|
| 🎵 **Three sources** | Spotify (Premium), YouTube Music, and local files (FLAC, ALAC, WAV, AIFF, MP3, AAC, Ogg) |
| 🎚️ **Real audio path** | Ten-band EQ, balance and a limiter, with no hidden processing |
| 💎 **Bit-perfect** | Exclusive output on Windows plays the file's own sample rate, untouched |
| 🏷️ **Honest labels** | Shows what you're really hearing: source, output and any processing |
| 📝 **Synced lyrics** | Shown full-screen when you leave the player alone |
| 🎨 **Skins** | Bring your own classic Winamp (`.wsz`) or Windows Media Player (`.wmz`) skins |
| 📱 **Spotify Connect** | Play to Whipspot from your phone |

<p>
  <img src=".github/readme/lyrics.jpg" width="49%" alt="Lyrics view">
  <img src=".github/readme/equalizer.jpg" width="49%" alt="Equalizer">
</p>

## How the sound flows

Every source goes through the same path. Each step only runs when it's needed. With the EQ off and the volume at 100, nothing changes the music.

```mermaid
flowchart LR
    A[Spotify<br>YouTube Music<br>Local files] --> B[Decode]
    B --> C[Resample<br><i>only if needed</i>]
    C --> D[EQ]
    D --> G[Balance · volume<br>limiter]
    G --> E[Output<br><i>exclusive or shared</i>]
    D -.-> F[Spectrum]
```

## Install

Get the latest build from [**Releases**](../../releases).

- **Windows:** unzip and run `whipspot.exe`. If SmartScreen warns you, click *More info* → *Run anyway*.
- **macOS:** unzip and move `Whipspot.app` to Applications. The app isn't notarised yet, so right-click it and choose *Open* the first time.

## Getting started

Open **Settings** (⚙️ or `Ctrl+,`) → **Accounts**.

- **Spotify:** log in for playback. To browse your library, create a free app on the [Spotify developer dashboard](https://developer.spotify.com/dashboard) with redirect URI `http://127.0.0.1:8899/callback`, then paste its Client ID into Whipspot.
- **YouTube Music:** click *Install* to get the stream helper ([yt-dlp](https://github.com/yt-dlp/yt-dlp)), then point Whipspot at a `cookies.txt` exported from your browser.
- **Local files:** nothing to set up. Your Music folder is added automatically; add more in Settings.

Your logins are kept in your system's password store, never in plain files.

## Skins

Whipspot comes with no skins. Drop your own `.wsz` or `.wmz` files into `~/.whipspot/skins` and pick one in **Settings → Skins**.

Find skins at the [Winamp Skin Museum](https://skins.webamp.org/) or the [WMP Skins Archive](https://wmpskinsarchive.neocities.org/).

Press `Ctrl+Shift+N` to go back to the normal look.

## Build from source

```
cargo run --release -p whipspot-app
```

Want to try it without any account? `cargo run -p whipspot-app --example demo` runs the player on made-up music.

## Good to know

- Whipspot is **unofficial** and not affiliated with Spotify, YouTube or Microsoft.
- Spotify playback uses [librespot](https://github.com/librespot-org/librespot), and YouTube Music uses your browser cookies. Both go against those services' terms, so use an account you can afford to lose.
- No skins, logos or other third-party assets are included.

## Licence

[Apache 2.0](LICENSE). Bundled fonts and vendored crates keep their own licences.
