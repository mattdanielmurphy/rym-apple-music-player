# RYM Apple Music Player

A Tauri v2 desktop application for macOS that displays RateYourMusic.com scores within the Apple Music web player interface.

## Features

### MVP (Current Implementation)
- ✅ **RYM Score Lookup**: Manual lookup interface to search for album ratings from RateYourMusic
- ✅ **Local Caching**: SQLite database caches ratings to minimize repeated scraping
- ✅ **Web Scraping**: Automated scraping of RYM album pages for ratings and review counts
- ✅ **Apple Music Window**: Separate webview window for Apple Music web player
- ✅ **Clean UI**: Modern, gradient-styled score badges with rating counts

### Future Enhancements
- 🔄 Automatic album detection from Apple Music DOM
- 🔄 Overlay score badges directly on Apple Music interface
- 🔄 Background webview for invisible RYM scraping
- 🔄 Rate limiting and respectful scraping delays
- 🔄 Enhanced error handling and retry logic

## Tech Stack

- **Frontend**: TypeScript, HTML, CSS
- **Backend**: Rust
  - `tauri` - Desktop app framework
  - `rusqlite` - SQLite database
  - `reqwest` - HTTP client
  - `scraper` - HTML parsing
  - `tokio` - Async runtime
- **Build Tool**: Vite
- **Package Manager**: npm

## Project Structure

```
/src-tauri/
  /src/
    main.rs         - Entry point
    lib.rs          - Tauri app setup and IPC commands
    database.rs     - SQLite database module for caching
    scraper.rs      - RYM web scraping module
  Cargo.toml        - Rust dependencies
  tauri.conf.json   - Tauri configuration
/src/
  main.ts           - Frontend TypeScript logic
  styles.css        - UI styles
index.html          - Main HTML structure
```

## Getting Started

### Prerequisites
- Node.js (v16 or higher)
- Rust (latest stable)
- macOS (for development)

### Installation

1. Clone the repository:
```bash
cd /path/to/rym-apple-music-player
```

2. Install dependencies:
```bash
npm install
```

3. Run the development server:
```bash
npm run tauri dev
```

### Building for Production

```bash
npm run tauri build
```

The built application will be in `src-tauri/target/release/bundle/`.

## Usage

1. Launch the application
2. A separate Apple Music window will open automatically
3. In the main window, use the test interface to look up album ratings:
   - Enter the artist name
   - Enter the album name
   - Click "Lookup RYM Score"
4. The app will search RYM and display the rating (if found)
5. Results are cached locally for faster subsequent lookups

## How It Works

### Data Flow

1. **User Input**: User enters artist and album name
2. **Cache Check**: App checks SQLite database for cached rating
3. **Web Scraping** (if not cached):
   - Constructs RYM search URL
   - Fetches search results page
   - Extracts first album result
   - Fetches album detail page
   - Parses rating and review count
4. **Caching**: Stores result in SQLite database
5. **Display**: Shows rating in UI with gradient badge

### Database Schema

```sql
CREATE TABLE album_ratings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    album_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    rym_rating REAL NOT NULL,
    rating_count INTEGER NOT NULL,
    rym_url TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    UNIQUE(album_name, artist_name)
);
```

## Development Notes

- **Respectful Scraping**: Only scrapes one page at a time, at human browsing speed
- **Error Handling**: Gracefully handles missing albums or network errors
- **Type Safety**: Full TypeScript on frontend, Rust on backend
- **State Management**: Uses Tauri's state management for database connection

## Known Limitations

- Currently requires manual input (automatic detection coming in future iteration)
- RYM scraping depends on their current HTML structure (may break if they update their site)
- macOS only (could be extended to Windows/Linux)
- No rate limiting implemented yet (be respectful when testing)

## Contributing

This is a personal project, but suggestions and improvements are welcome!

## License

MIT License - feel free to use and modify as needed.

## Acknowledgments

- Built with [Tauri](https://tauri.app/)
- Data sourced from [RateYourMusic](https://rateyourmusic.com/)
- Apple Music web player by Apple Inc.
