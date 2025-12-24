# RYM Apple Music Player - Context

## Project Overview
Tauri v2 desktop application for macOS that displays RateYourMusic.com scores within the Apple Music web player interface.

## Architecture
- **Dual Window Architecture**: Two persistent WebviewWindows managed by Rust.
  - **Music Window**: Displays `https://music.apple.com` (Visible by default).
  - **RYM Window**: Displays `https://rateyourmusic.com` (Hidden by default).
- **Tab Switching**: Injected JavaScript UI that toggles window visibility via Tauri IPC commands (`show_music`, `show_rym`).
- **Backend**: Rust for window management, IPC commands, and SQLite caching.
- **State Persistence**: Both windows maintain independent session state, cookies, and scroll positions.
- **Storage**: SQLite database for caching RYM scores locally.

## Tech Stack
- Tauri v2
- Rust backend with:
  - `rusqlite` for database
  - `reqwest` for HTTP requests
  - `scraper` for HTML parsing
  - `tokio` for async runtime
- TypeScript frontend
- Native macOS WebKit webviews
- SQLite storage (stored in app data directory)

## Project Structure
```
/src-tauri/
  /src/
    main.rs         - Entry point
    lib.rs          - Tauri app setup and IPC commands
    database.rs     - SQLite database module
    scraper.rs      - RYM web scraping module
  Cargo.toml        - Rust dependencies
  tauri.conf.json   - Tauri configuration
/src/
  main.ts           - Frontend TypeScript logic
  styles.css        - UI styles
index.html          - Main HTML structure
```

## Development Commands
- `npm install` - Install dependencies
- `npm run tauri dev` - Run development server
- `npm run tauri build` - Build production app

# **Conventions**
- Prefix all new Database/Supabase tables with `RYM-APPLE-MUSIC-PLAYER_` (e.g., `RYM-APPLE-MUSIC-PLAYER_ratings`).
- Use `mv [path] ~/.Trash/` instead of `rm`.

## Key Features Implemented (MVP)
1. ✅ SQLite database for caching RYM ratings
2. ✅ RYM web scraper (searches and extracts ratings)
3. ✅ IPC command `get_rym_rating` for fetching ratings
4. ✅ Dual-window architecture with tab switching
5. ✅ Injected UI for seamless navigation between Apple Music and RYM
6. ✅ Separate Apple Music webview window
7. ✅ Standard macOS Edit menu and keyboard shortcuts
8. ✅ Browser-style Back/Forward navigation (Cmd+[ / Cmd+])
9. ✅ RYM Metadata Injection: Real-time insights injected into Apple Music UI.
10. ✅ Automatic RYM to Apple Music synchronization.
11. ✅ Cmd-F to focus search bars in both windows.
12. ✅ Manual "Sync to RYM" feature from Apple Music.
13. ✅ RYM auto-clicker for sync searches.
14. ✅ Enhanced Debug Logging: Detailed logs for music extraction and scraping.

## Next Steps (Future Iterations)
- Inject JavaScript into Apple Music webview to detect currently playing album
- Display score badges overlaid on Apple Music UI
- Improve RYM scraping reliability
- Add rate limiting and respectful scraping delays
- Automatic background sync when track changes on Apple Music

## Recent Updates
- **Database-First Sync**: All sync operations now check local SQLite first, then Supabase, then fall back to search
- **Immediate UI Updates**: Cached RYM data is immediately broadcast to Apple Music UI when found in database
- **Rate Limiting**: 2-second minimum delay between RYM page loads to prevent automation detection
  - All RYM navigation now goes through `navigate_to_rym_with_rate_limit()` helper
  - Tracks last navigation timestamp to enforce delays
- **Lazy RYM Loading**: RYM window starts with `about:blank` and only loads RYM homepage when user first switches to RYM tab
- **Comprehensive Logging**: All database operations, cache hits/misses, and sync steps are logged with prefixes:
  - `RYM-SYNC:` - Sync to RYM operations
  - `RYM-GET-RATING:` - Rating retrieval operations
  - `RYM-SAVE-RATING:` - Rating save operations
  - `RYM-DATABASE:` - Database operations
  - `RYM-SUPABASE:` - Supabase operations
  - `RYM-RATE-LIMIT:` - Rate limiting actions
  - `RYM-INIT:` - Window initialization
