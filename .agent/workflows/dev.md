---
description: Run the development server
---

# Development Workflow

This workflow runs the Tauri development server with hot-reload enabled.

## Steps

1. Ensure you're in the project root directory

2. Install dependencies (if not already installed):
```bash
npm install
```

// turbo
3. Run the development server:
```bash
npm run tauri dev
```

This will:
- Start the Vite development server for the frontend
- Compile the Rust backend
- Launch the application with hot-reload enabled
- Open the main window with the test UI
- Open a separate Apple Music window

## What to Expect

- **Main Window**: Test interface for manual RYM lookups
- **Apple Music Window**: Separate window loading music.apple.com
- **Status Bar**: Bottom bar showing current operation status
- **Console**: Rust backend logs in the terminal

## Development Tips

- Frontend changes (TypeScript, HTML, CSS) will hot-reload automatically
- Backend changes (Rust) will trigger a recompile
- Check the terminal for any compilation errors or runtime logs
- The SQLite database is stored in the app data directory
