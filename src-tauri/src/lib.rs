mod database;
mod scraper;
mod supabase;

use database::{AlbumRating, Database};
use std::sync::Mutex;
use tauri::{Emitter, Manager, State, window::Color, menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem, Submenu}};
use supabase::SupabaseClient;

// Application state to hold the database connection and Supabase client
pub struct AppState {
    db: Mutex<Database>,
    supabase: Option<SupabaseClient>,
    pending_apple_music_url: Mutex<Option<String>>,
    // Track what each window is currently displaying to prevent redundant syncs
    music_current_info: Mutex<Option<String>>, // "Artist - Album"
    rym_current_info: Mutex<Option<String>>,    // "Artist - Album"
    current_music_url: Mutex<Option<String>>,   // Exact URL currently on AM, for ping-pong prevention
    // Rate limiting for RYM navigation (timestamp in milliseconds)
    last_rym_navigation: Mutex<Option<u128>>,
    rym_initialized: Mutex<bool>, // Track if RYM window has been loaded at least once
    prevent_next_am_sync: Mutex<bool>, // Force blocking of the next sync from AM to RYM
}

// IPC Command to get RYM rating for an album
#[tauri::command]
async fn get_rym_rating(
    artist: String,
    album: String,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<Option<AlbumRating>, String> {
    println!("RYM-GET-RATING: ========================================");
    println!("RYM-GET-RATING: Request for: {} - {}", artist, album);
    
    // First, check the cache
    println!("RYM-GET-RATING: Checking local SQLite cache...");
    let cached_rating = {
        let db = state.db.lock().unwrap();
        db.get_rating(&album, &artist).ok().flatten()
    };
    
    if let Some(rating) = cached_rating {
        println!("RYM-GET-RATING: ✓ LOCAL CACHE HIT!");
        println!("RYM-GET-RATING:   - URL: {}", rating.rym_url);
        println!("RYM-GET-RATING:   - Rating: {}", rating.rym_rating);
        println!("RYM-GET-RATING:   - Genres: {}", rating.genres);
        println!("RYM-GET-RATING:   - Release Date: {}", rating.release_date);
        println!("RYM-GET-RATING: ========================================");
        return Ok(Some(rating));
    }
    
    println!("RYM-GET-RATING: ❌ Local cache miss");
    
    // Check Supabase if local miss
    println!("RYM-GET-RATING: Checking Supabase cache...");
    if let Some(supabase) = &state.supabase {
        if let Some(rating) = supabase.get_cached_rating(&artist, &album).await {
            println!("RYM-GET-RATING: ✓ SUPABASE CACHE HIT!");
            println!("RYM-GET-RATING:   - URL: {}", rating.rym_url);
            println!("RYM-GET-RATING:   - Rating: {}", rating.rym_rating);
            println!("RYM-GET-RATING:   - Genres: {}", rating.genres);
            println!("RYM-GET-RATING:   - Release Date: {}", rating.release_date);
            
            // Save to local cache for next time
            println!("RYM-GET-RATING: Saving to local cache...");
            let _ = state.db.lock().unwrap().save_rating(&rating);
            
            // Broadcast for UI
            let _ = app.emit("rym-rating-updated", rating.clone());
            
            println!("RYM-GET-RATING: ========================================");
            return Ok(Some(rating));
        }
    } else {
        println!("RYM-GET-RATING: Supabase client not configured");
    }
    
    println!("RYM-GET-RATING: ❌ Supabase cache miss");
    
    // If not in cache, trigger the hidden scraper window
    println!("RYM-GET-RATING: Triggering RYM search...");
    
    // The foreground "rym" window is used for searching
    let query = format!("{} {}", artist, album);
    let encoded_query = urlencoding::encode(&query);
    let search_url = format!("https://rateyourmusic.com/search?searchterm={}&searchtype=l", encoded_query);
    
    println!("RYM-GET-RATING: Search URL: {}", search_url);
    
    navigate_to_rym_with_rate_limit(&app, search_url).await?;
    
    println!("RYM-GET-RATING: ✓ Search initiated");
    println!("RYM-GET-RATING: ========================================");
    Ok(None)
}

// IPC Command to save a rating received from the scraper
#[tauri::command]
async fn save_rym_rating(
    rating: AlbumRating,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    println!("RYM-SAVE-RATING: ========================================");
    println!("RYM-SAVE-RATING: Received scraped data from RYM");
    println!("RYM-SAVE-RATING:   - Artist: {}", rating.artist_name);
    println!("RYM-SAVE-RATING:   - Album: {}", rating.album_name);
    println!("RYM-SAVE-RATING:   - URL: {}", rating.rym_url);
    println!("RYM-SAVE-RATING:   - Rating: {}", rating.rym_rating);
    println!("RYM-SAVE-RATING:   - Rating Count: {}", rating.rating_count);
    println!("RYM-SAVE-RATING:   - Genres: {}", rating.genres);
    println!("RYM-SAVE-RATING:   - Release Date: {}", rating.release_date);
    
    // Clear loop prevention on RYM side if this is a fresh extraction
    if let Some(rym_window) = app.get_webview_window("rym") {
        let _ = rym_window.eval("localStorage.removeItem('tauri_ignore_next_sync')");
        println!("RYM-SAVE-RATING: Cleared sync loop prevention flag");
    }

    // Save to local SQLite database
    println!("RYM-SAVE-RATING: Saving to local SQLite database...");
    {
        let db = state.db.lock().unwrap();
        db.save_rating(&rating)
            .map_err(|e| {
                eprintln!("RYM-SAVE-RATING: ❌ Failed to save to local database: {}", e);
                format!("Failed to save rating locally: {}", e)
            })?;
    }
    println!("RYM-SAVE-RATING: ✓ Saved to local database");
    
    // Broadcast for the AM window to pick up
    println!("RYM-SAVE-RATING: Broadcasting to Apple Music UI...");
    let _ = app.emit("rym-rating-updated", rating.clone());
    println!("RYM-SAVE-RATING: ✓ Broadcast complete");

    // Update RYM state in protector
    {
        let mut rym_info = state.rym_current_info.lock().unwrap();
        *rym_info = Some(format!("{} - {}", rating.artist_name, rating.album_name));
        println!("RYM-SAVE-RATING: ✓ Updated RYM state tracker");
    }

    // Save to Supabase asynchronously
    if let Some(supabase) = &state.supabase {
        println!("RYM-SAVE-RATING: Initiating async Supabase save...");
        let r = rating.clone();
        let client = supabase.clone();
        tokio::spawn(async move {
            println!("RYM-SUPABASE: Attempting to save: {} - {}", r.artist_name, r.album_name);
            match client.save_rating(&r).await {
                Ok(_) => println!("RYM-SUPABASE: ✓ Successfully saved to Supabase"),
                Err(e) => eprintln!("RYM-SUPABASE: ❌ Failed to save: {}", e),
            }
        });
    } else {
        println!("RYM-SAVE-RATING: Supabase client not configured, skipping cloud save");
    }
    
    println!("RYM-SAVE-RATING: ========================================");
    Ok(())
}

// IPC Command to manually link a specific RYM page to an AM Artist/Album
#[tauri::command]
async fn set_manual_match(
    target_artist: String,
    target_album: String,
    rating: AlbumRating,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    println!("RYM-MANUAL-MATCH: ========================================");
    println!("RYM-MANUAL-MATCH: Manual link requested!");
    println!("RYM-MANUAL-MATCH:   - Target (AM): {} - {}", target_artist, target_album);
    println!("RYM-MANUAL-MATCH:   - Source (RYM): {} - {} ({})", rating.artist_name, rating.album_name, rating.rym_url);

    // Create a new rating object that uses the TARGET artist/album as the primary keys
    // This ensures that when AM asks for "Artist A - Album X", it finds this entry.
    // We keep the rest of the data (URL, rating, etc.) from the actual RYM page.
    let mut linked_rating = rating.clone();
    linked_rating.artist_name = target_artist.clone();
    linked_rating.album_name = target_album.clone();
    
    // Save to local SQLite database
    println!("RYM-MANUAL-MATCH: Saving link to local database...");
    {
        let db = state.db.lock().unwrap();
        db.save_rating(&linked_rating)
            .map_err(|e| {
                eprintln!("RYM-MANUAL-MATCH: ❌ Failed to save: {}", e);
                format!("Failed to save manual match: {}", e)
            })?;
    }
    
    // Broadcast update so AM UI reflects the new correct data immediately
    println!("RYM-MANUAL-MATCH: Broadcasting update...");
    let _ = app.emit("rym-rating-updated", linked_rating.clone());

    // Also save to Supabase (optional, but good for persistence)
    if let Some(supabase) = &state.supabase {
        let r = linked_rating.clone();
        let client = supabase.clone();
        tokio::spawn(async move {
            let _ = client.save_rating(&r).await;
        });
    }

    println!("RYM-MANUAL-MATCH: ✓ Link established successfully");
    println!("RYM-MANUAL-MATCH: ========================================");
    Ok(())
}


#[tauri::command]
fn start_drag(window: tauri::Window) {
    let _ = window.start_dragging();
}

// Helper function to navigate to RYM with rate limiting
async fn navigate_to_rym_with_rate_limit(
    app: &tauri::AppHandle,
    url: String,
) -> Result<(), String> {
    const MIN_DELAY_MS: u128 = 2000; // 2 seconds minimum between RYM page loads
    
    println!("RYM-RATE-LIMIT: Request to navigate to: {}", url);
    
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    
    // Calculate wait time in a separate scope to drop the lock
    let wait_time = {
        let state = app.state::<AppState>();
        let last_nav = state.last_rym_navigation.lock().unwrap();
        
        if let Some(last_time) = *last_nav {
            let elapsed = now - last_time;
            if elapsed < MIN_DELAY_MS {
                Some(MIN_DELAY_MS - elapsed)
            } else {
                None
            }
        } else {
            None
        }
    }; // Lock is dropped here
    
    // Sleep if needed (without holding any locks)
    if let Some(delay) = wait_time {
        println!("RYM-RATE-LIMIT: Delaying navigation by {}ms to respect rate limits", delay);
        tokio::time::sleep(tokio::time::Duration::from_millis(delay as u64)).await;
    }
    
    // Update the last navigation time
    {
        let state = app.state::<AppState>();
        let mut last_nav = state.last_rym_navigation.lock().unwrap();
        *last_nav = Some(now);
    } // Lock is dropped here
    
    println!("RYM-RATE-LIMIT: Getting RYM window...");
    let rym_window = app.get_webview_window("rym")
        .ok_or_else(|| {
            println!("RYM-RATE-LIMIT: ❌ ERROR - RYM window not found!");
            "RYM window not found".to_string()
        })?;
    
    println!("RYM-RATE-LIMIT: Navigating now...");
    rym_window.navigate(url.parse().unwrap())
        .map_err(|e| {
            let err_msg = format!("Failed to navigate: {}", e);
            println!("RYM-RATE-LIMIT: ❌ ERROR - {}", err_msg);
            err_msg
        })?;
    
    println!("RYM-RATE-LIMIT: ✓ Navigation successful");
    Ok(())
}

#[tauri::command]
fn show_music(app: tauri::AppHandle) {
    if let Some(m) = app.get_webview_window("music") {
        let _ = m.show();
        let _ = m.set_focus();
    }
    if let Some(r) = app.get_webview_window("rym") {
        let _ = r.hide();
    }
}

#[tauri::command]
async fn show_rym(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    println!("RYM-SHOW: ========================================");
    println!("RYM-SHOW: show_rym command called");
    
    if let Some(r) = app.get_webview_window("rym") {
        println!("RYM-SHOW: ✓ RYM window found");
        
        // Check if this is the first time showing RYM window
        let needs_init = {
            let mut initialized = state.rym_initialized.lock().unwrap();
            let needs_init = !*initialized;
            if needs_init {
                println!("RYM-INIT: First time showing RYM window, loading homepage...");
                *initialized = true;
            } else {
                println!("RYM-SHOW: RYM already initialized, skipping navigation");
            }
            needs_init
        }; // Lock is dropped here
        
        if needs_init {
            println!("RYM-SHOW: Calling navigate_to_rym_with_rate_limit...");
            // Navigate to RYM homepage with rate limiting
            match navigate_to_rym_with_rate_limit(&app, "https://rateyourmusic.com".to_string()).await {
                Ok(_) => println!("RYM-SHOW: ✓ Navigation completed successfully"),
                Err(e) => {
                    println!("RYM-SHOW: ❌ Navigation failed: {}", e);
                    return Err(e);
                }
            }
        }
        
        println!("RYM-SHOW: Showing RYM window and hiding Music window...");
        let _ = r.show();
        let _ = r.set_focus();
        println!("RYM-SHOW: ✓ RYM window shown and focused");
    } else {
        println!("RYM-SHOW: ❌ ERROR - RYM window not found!");
        println!("RYM-SHOW: ========================================");
        return Err("RYM window not found".to_string());
    }
    
    if let Some(m) = app.get_webview_window("music") {
        let _ = m.hide();
        println!("RYM-SHOW: ✓ Music window hidden");
    }
    
    println!("RYM-SHOW: ========================================");
    Ok(())
}

#[tauri::command]
fn set_pending_music_url(url: String, artist: Option<String>, album: Option<String>, state: State<'_, AppState>, app: tauri::AppHandle) {
    println!("RYM-APPLE-MUSIC: Received sync URL: {}", url);
    
    // CONSTRUCT KEY
    let metadata_key = if let (Some(a), Some(b)) = (&artist, &album) {
        Some(format!("{} - {}", a, b))
    } else {
        None
    };

    // --- HARD SYNC BLOCKER ---
    // We are about to force a navigation to AM. We MUST ignore the sync request that AM will
    // inevitably send back when it loads this album.
    {
        let mut prevent = state.prevent_next_am_sync.lock().unwrap();
        *prevent = true;
        println!("RYM-APPLE-MUSIC: 🛡️ ACTIVATED HARD SYNC BLOCKER (prevent_next_am_sync = true)");
    }
    
    if let Some(key) = &metadata_key {
        println!("RYM-APPLE-MUSIC: Sync request metadata: {}", key);
        
        let current_info = state.music_current_info.lock().unwrap();
        if let Some(current) = &*current_info {
            println!("RYM-APPLE-MUSIC: Current AM state: {}", current);
            // Allow loose matching (contains) or exact
            if current == key || current.contains(key) || key.contains(current) {
                println!("RYM-APPLE-MUSIC: ❌ Ignoring sync request - AM is already on this Album (Metadata Match)");
                // Even if we skip navigation, we should ensure RYM state is updated to this album
                // since the user is clearly looking at it on RYM
                let mut rym_info = state.rym_current_info.lock().unwrap();
                *rym_info = Some(key.clone());
                return;
            }
        }
    }
    
    // Fallback: Check if we are already on this URL (legacy/backup check)
    {
        let current_url_opt = state.current_music_url.lock().unwrap();
        if let Some(current_url) = &*current_url_opt {
             println!("RYM-APPLE-MUSIC: Checking against current URL: {}", current_url);
             println!("RYM-APPLE-MUSIC: Incoming URL: {}", url);
             
             // Simple string equality check
            if current_url == &url {
                println!("RYM-APPLE-MUSIC: ❌ Ignoring sync request - AM is already on this URL (ping-pong prevention)");
                return;
            }
            
            // Normalize check: handle geo.music vs music.apple
            let normalized_current = current_url.replace("geo.music.apple.com", "music.apple.com");
            let normalized_incoming = url.replace("geo.music.apple.com", "music.apple.com");
            
            if normalized_current == normalized_incoming {
                 println!("RYM-APPLE-MUSIC: ❌ Ignoring sync request - URLs match after normalization");
                 return;
            }
        } else {
            println!("RYM-APPLE-MUSIC: No current music URL set.");
        }
    }

    let mut pending = state.pending_apple_music_url.lock().unwrap();
    
    // Only proceed if it's a new URL
    if *pending == Some(url.clone()) { return; }
    
    *pending = Some(url.clone());
    
    // Update current music URL since we are navigating there
    let mut current_url = state.current_music_url.lock().unwrap();
    *current_url = Some(url.clone());

    // CRITICAL FIX: Update RYM state tracker.
    // Since this request comes FROM RYM (user clicked a link or is on a page),
    // we know RYM is displaying this album. Updating this state prevents the
    // "piong-pong" loop where AM loads -> notices album -> syncs back to RYM.
    if let Some(key) = &metadata_key {
        let mut rym_info = state.rym_current_info.lock().unwrap();
        *rym_info = Some(key.clone());
        println!("RYM-APPLE-MUSIC: ✓ Updated RYM state tracker to prevent loop: {}", key);
    }
    
    // Navigate immediately so it's ready when the user switches
    if let Some(m) = app.get_webview_window("music") {
        if let Ok(parsed_url) = url.parse() {
            // Tell the music window to ignore the NEXT sync detection to prevent loops
            let _ = m.eval("localStorage.setItem('tauri_ignore_next_sync', 'true')");
            let _ = m.navigate(parsed_url);
            
            // Trigger notification in Music window so user knows it's ready
            let _ = m.eval("if (window.showSyncToast) window.showSyncToast('Synced from RYM')");
            
            // Also trigger in RYM window if that's where they are
            if let Some(r) = app.get_webview_window("rym") {
               let _ = r.eval("if (window.showSyncToast) window.showSyncToast('Synced to Apple Music')");
            }
        }
    }
}

#[tauri::command]
async fn sync_to_rym(artist: String, album: String, background: bool, music_url: Option<String>, state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    let album_key = format!("{} - {}", artist, album);
    
    println!("RYM-SYNC: ========================================");
    println!("RYM-SYNC: Starting sync to RYM for: {}", album_key);
    println!("RYM-SYNC: Background mode: {}", background);
    if let Some(url) = &music_url {
        println!("RYM-SYNC: Source Music URL: {}", url);
    }
    
    // 0. CHECK HARD SYNC BLOCKER
    let skip_rym_navigation = {
        let mut prevent = state.prevent_next_am_sync.lock().unwrap();
        if *prevent {
            println!("RYM-SYNC: 🛑 Hard Sync Blocker active - Will skip RYM navigation but allow UI update");
            *prevent = false;
            true
        } else {
            false
        }
    };
    
    // LOOP PREVENTION: If RYM window already has this album, skip.
    {
        let rym_info = state.rym_current_info.lock().unwrap();
        if let Some(current) = &*rym_info {
            // Relaxed check: Exact match OR Contains OR Case-insensitive
            let match_found = current == &album_key 
                || current.contains(&album_key) 
                || album_key.contains(current)
                || current.to_lowercase() == album_key.to_lowercase();
                
            if match_found {
                println!("RYM-SYNC: ❌ Skipping sync - RYM window already has {} (Match: {})", album_key, current);
                println!("RYM-SYNC: ========================================");
                return Ok(());
            }
        }
    }

    // Update Music state in protector
    {
        let mut music_info = state.music_current_info.lock().unwrap();
        *music_info = Some(album_key.clone());
        println!("RYM-SYNC: ✓ Updated music state tracker");
        
        // Update current music URL for ping-pong prevention
        if let Some(url) = &music_url {
            let mut current_url = state.current_music_url.lock().unwrap();
            *current_url = Some(url.clone());
        }
        
        // Also update RYM state since we are sending it there
        let mut rym_info = state.rym_current_info.lock().unwrap();
        *rym_info = Some(album_key.clone());
        
        // Mark RYM as initialized since we are about to navigate it
        let mut init = state.rym_initialized.lock().unwrap();
        *init = true;
    }

    if let Some(rym_window) = app.get_webview_window("rym") {
        // STEP 1: Check local SQLite database first
        println!("RYM-SYNC: Step 1 - Checking local SQLite database...");
        let cached_rating = {
            let db = state.db.lock().unwrap();
            db.get_rating(&album, &artist).ok().flatten()
        };

        if let Some(rating) = &cached_rating {
            println!("RYM-SYNC: ✓ LOCAL CACHE HIT!");
            println!("RYM-SYNC:   - URL: {}", rating.rym_url);
            println!("RYM-SYNC:   - Rating: {}", rating.rym_rating);
            println!("RYM-SYNC:   - Genres: {}", rating.genres);
            println!("RYM-SYNC:   - Release Date: {}", rating.release_date);
            
            // Immediately broadcast the cached data to Apple Music UI
            println!("RYM-SYNC: Broadcasting cached data to Apple Music UI...");
            let _ = app.emit("rym-rating-updated", rating.clone());
            
            // Navigate to the cached URL
            if !skip_rym_navigation {
                println!("RYM-SYNC: Navigating to cached URL...");
                let _ = rym_window.eval("localStorage.setItem('tauri_ignore_next_sync', 'true')");
                
                // Propagate the target album key to RYM window for "Set Match" functionality
                let safe_key = album_key.replace("'", "\\'");
                let js_key = format!("localStorage.setItem('tauri_last_synced_album', '{}')", safe_key);
                let _ = rym_window.eval(&js_key);
                let _ = rym_window.eval("localStorage.setItem('tauri_sync_occurred', 'true')");
                
                // Also set the last pending URL to the source URL to prevent JS race conditions
                if let Some(src_url) = &music_url {
                     // Escape single quotes for JS string
                     let safe_url = src_url.replace("'", "\\'");
                     let js = format!("localStorage.setItem('tauri_last_pending_url', '{}')", safe_url);
                     let _ = rym_window.eval(&js);
                }

                navigate_to_rym_with_rate_limit(&app, rating.rym_url.clone()).await?;
            } else {
                println!("RYM-SYNC: ⚠️ Skipping RYM navigation (Loop Prevention active)");
            }
            
            println!("RYM-SYNC: ✓ Fast sync complete (local cache)");
        } else {
            println!("RYM-SYNC: ❌ Local cache miss");
            
            // STEP 2: Check Supabase if not in local
            println!("RYM-SYNC: Step 2 - Checking Supabase...");
            let mut sb_rating = None;
            if let Some(supabase) = &state.supabase {
                sb_rating = supabase.get_cached_rating(&artist, &album).await;
            } else {
                println!("RYM-SYNC: Supabase client not configured");
            }

            if let Some(rating) = sb_rating {
                println!("RYM-SYNC: ✓ SUPABASE CACHE HIT!");
                println!("RYM-SYNC:   - URL: {}", rating.rym_url);
                println!("RYM-SYNC:   - Rating: {}", rating.rym_rating);
                println!("RYM-SYNC:   - Genres: {}", rating.genres);
                println!("RYM-SYNC:   - Release Date: {}", rating.release_date);
                
                // Save to local cache for next time
                println!("RYM-SYNC: Saving Supabase data to local cache...");
                {
                    let db = state.db.lock().unwrap();
                    if let Err(e) = db.save_rating(&rating) {
                        eprintln!("RYM-SYNC: ⚠️  Failed to save to local cache: {}", e);
                    } else {
                        println!("RYM-SYNC: ✓ Saved to local cache");
                    }
                }
                
                // Immediately broadcast the cached data to Apple Music UI
                println!("RYM-SYNC: Broadcasting cached data to Apple Music UI...");
                let _ = app.emit("rym-rating-updated", rating.clone());
                
                // Navigate to the cached URL
                if !skip_rym_navigation {
                    println!("RYM-SYNC: Navigating to cached URL...");
                    let _ = rym_window.eval("localStorage.setItem('tauri_ignore_next_sync', 'true')");
                
                // Propagate the target album key to RYM window for "Set Match" functionality
                let safe_key = album_key.replace("'", "\\'");
                let js_key = format!("localStorage.setItem('tauri_last_synced_album', '{}')", safe_key);
                let _ = rym_window.eval(&js_key);
                let _ = rym_window.eval("localStorage.setItem('tauri_sync_occurred', 'true')");
                    
                    if let Some(src_url) = &music_url {
                         let safe_url = src_url.replace("'", "\\'");
                         let js = format!("localStorage.setItem('tauri_last_pending_url', '{}')", safe_url);
                         let _ = rym_window.eval(&js);
                    }
                    
                    navigate_to_rym_with_rate_limit(&app, rating.rym_url.clone()).await?;
                } else {
                    println!("RYM-SYNC: ⚠️ Skipping RYM navigation (Loop Prevention active)");
                }
                
                println!("RYM-SYNC: ✓ Fast sync complete (Supabase cache)");
            } else {
                println!("RYM-SYNC: ❌ Supabase cache miss");
                
                // STEP 3: FALLBACK - DuckDuckGo "I'm Feeling Lucky" Search
                println!("RYM-SYNC: Step 3 - Falling back to DuckDuckGo search...");
                let query = format!("\\ site:rateyourmusic.com/release {} {}", artist, album);
                let encoded_query = urlencoding::encode(&query);
                // DDG URL
                let search_url = format!("https://duckduckgo.com/?q={}", encoded_query);
                
                if !skip_rym_navigation {
                    println!("RYM-SYNC: Search query: {}", query);
                    println!("RYM-SYNC: Search URL: {}", search_url);
                    
                    let _ = rym_window.eval("localStorage.setItem('tauri_ignore_next_sync', 'true')");
                
                // Propagate the target album key to RYM window for "Set Match" functionality
                let safe_key = album_key.replace("'", "\\'");
                let js_key = format!("localStorage.setItem('tauri_last_synced_album', '{}')", safe_key);
                let _ = rym_window.eval(&js_key);
                let _ = rym_window.eval("localStorage.setItem('tauri_sync_occurred', 'true')");
                
                // Set flags to indicate we are doing a DDG sync and a sync just happened
                let _ = rym_window.eval("localStorage.setItem('tauri_ddg_sync_active', 'true')");
                let _ = rym_window.eval("localStorage.setItem('tauri_sync_occurred', 'true')");
                    
                    if let Some(src_url) = &music_url {
                         let safe_url = src_url.replace("'", "\\'");
                         let js = format!("localStorage.setItem('tauri_last_pending_url', '{}')", safe_url);
                         let _ = rym_window.eval(&js);
                    }
                    
                    navigate_to_rym_with_rate_limit(&app, search_url).await?;
                } else {
                    println!("RYM-SYNC: ⚠️ Skipping RYM navigation (Loop Prevention active)");
                }
                
                println!("RYM-SYNC: ✓ Search initiated (DuckDuckGo)");
            }
        }
        
        // Show toast notifications
        if background {
            let msg = format!("if (window.showSyncToast) window.showSyncToast(`Synced: {}`)", album.replace("`", "\\`"));
            let _ = rym_window.eval(&msg);
            if let Some(m) = app.get_webview_window("music") {
                let _ = m.eval(&msg);
            }
        } else {
            // Show RYM window if not in background mode
            let _ = rym_window.show();
            let _ = rym_window.set_focus();
            if let Some(m) = app.get_webview_window("music") {
                let _ = m.hide();
            }
        }
    } else {
        println!("RYM-SYNC: ❌ ERROR - RYM window not found!");
        println!("RYM-SYNC: ========================================");
        return Err("RYM window not found".to_string());
    }
    
    println!("RYM-SYNC: ========================================");
    Ok(())
}

#[tauri::command]
async fn save_sample_html(page_type: String, url: String, html: String, app: tauri::AppHandle) -> Result<(), String> {
    let path = std::path::Path::new("/Users/matthewmurphy/projects/rym-apple-music-player/sample_pages");
    
    if !path.exists() {
        std::fs::create_dir_all(path).map_err(|e| e.to_string())?;
    }

    // Clean up type name for filename
    let safe_type = page_type.replace("/", "_").trim_start_matches('_').to_string();
    let fileName = format!("{}_sample.html", safe_type);

    let file_path = path.join(fileName);
    if !file_path.exists() {
        println!("RYM-APPLE-MUSIC: Saving new sample HTML for {} (URL: {})", page_type, url);
        let content_with_url = format!("<!-- Source URL: {} -->\n{}", url, html);
        std::fs::write(file_path, content_with_url).map_err(|e| e.to_string())?;
    }
    
    Ok(())
}

#[tauri::command]
fn go_back(app: tauri::AppHandle) {
    if let Some(w) = get_active_window(&app) {
        let _ = w.eval("window.history.back()");
    }
}

#[tauri::command]
fn go_forward(app: tauri::AppHandle) {
    if let Some(w) = get_active_window(&app) {
        let _ = w.eval("window.history.forward()");
    }
}

fn get_active_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    let m = app.get_webview_window("music")?;
    let r = app.get_webview_window("rym")?;
    if m.is_visible().unwrap_or(false) {
        Some(m)
    } else {
        Some(r)
    }
}

async fn toggle_windows(app: &tauri::AppHandle) {
    let music = app.get_webview_window("music");
    let rym = app.get_webview_window("rym");

    if let (Some(m), Some(_r)) = (music, rym) {
        let is_music_visible = m.is_visible().unwrap_or(false);
        if is_music_visible {
            // Switching to RYM - use the command to handle initialization
            let state = app.state::<AppState>();
            let _ = show_rym(app.clone(), state).await;
        } else {
            // Switching to Music
            show_music(app.clone());
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            // Load environment variables
            println!("RYM-INIT: Loading environment variables...");
            if let Err(e) = dotenvy::dotenv() {
                println!("RYM-INIT: Note - Standard .env not found or failed: {}", e);
            }
            
            // Also try to load .env.local from the project root (parent of src-tauri)
            let env_local_path = std::path::Path::new("../.env.local");
            match dotenvy::from_path(env_local_path) {
                Ok(_) => println!("RYM-INIT: ✓ Loaded ../.env.local"),
                Err(e) => println!("RYM-INIT: Note - Could not load ../.env.local: {}", e),
            }

            let app_handle = app.handle();
            let app_dir = app_handle.path().app_data_dir().expect("Failed to get app data dir");
            let _ = std::fs::create_dir_all(&app_dir);
            let db_path = app_dir.join("rym_bridge.db");
            
            let db = Database::new(db_path).expect("Failed to initialize database");
            
            let supabase = SupabaseClient::from_env();
            if supabase.is_some() {
                println!("RYM-INIT: ✓ Supabase client initialized");
            } else {
                println!("RYM-INIT: ⚠️ Supabase client failed to initialize (Missing keys?)");
            }

            app.manage(AppState {
                db: Mutex::new(db),
                supabase,
                pending_apple_music_url: Mutex::new(None),
                music_current_info: Mutex::new(None),
                rym_current_info: Mutex::new(None),
                current_music_url: Mutex::new(None),
                last_rym_navigation: Mutex::new(None),
                rym_initialized: Mutex::new(false),
                prevent_next_am_sync: Mutex::new(false),
            });

            let app_handle_clone = app_handle.clone();
            
            // Setup Native Menu with Shortcuts
            let toggle_shortcut = MenuItem::with_id(app, "toggle", "Switch Tabs", true, Some("CmdOrCtrl+Shift+["))?;
            let toggle_shortcut_alt = MenuItem::with_id(app, "toggle_alt", "Switch Tabs", true, Some("CmdOrCtrl+Shift+]"))?;
            let devtools_shortcut = MenuItem::with_id(app, "devtools", "Open DevTools", true, Some("CmdOrCtrl+Option+I"))?;
            let back_shortcut = MenuItem::with_id(app, "back", "Back", true, Some("CmdOrCtrl+["))?;
            let forward_shortcut = MenuItem::with_id(app, "forward", "Forward", true, Some("CmdOrCtrl+]"))?;
            let reload_shortcut = MenuItem::with_id(app, "reload", "Reload Page", true, Some("CmdOrCtrl+R"))?;
            
            let menu = Menu::with_items(app, &[
                &Submenu::with_items(app, "App", true, &[
                    &PredefinedMenuItem::about(app, None, Some(AboutMetadata::default()))?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::services(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::hide(app, None)?,
                    &PredefinedMenuItem::hide_others(app, None)?,
                    &PredefinedMenuItem::show_all(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::quit(app, None)?,
                ])?,
                &Submenu::with_items(app, "Edit", true, &[
                    &PredefinedMenuItem::undo(app, None)?,
                    &PredefinedMenuItem::redo(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::cut(app, None)?,
                    &PredefinedMenuItem::copy(app, None)?,
                    &PredefinedMenuItem::paste(app, None)?,
                    &PredefinedMenuItem::separator(app)?,
                    &PredefinedMenuItem::select_all(app, None)?,
                ])?,
                &Submenu::with_items(app, "View", true, &[
                    &toggle_shortcut,
                    &toggle_shortcut_alt,
                    &PredefinedMenuItem::separator(app)?,
                    &back_shortcut,
                    &forward_shortcut,
                    &reload_shortcut,
                    &PredefinedMenuItem::separator(app)?,
                    &devtools_shortcut,
                ])?,
            ])?;
            app.set_menu(menu)?;

            app.on_menu_event(move |app, event| {
                if event.id() == "toggle" || event.id() == "toggle_alt" {
                    let app_handle = app.clone();
                    tauri::async_runtime::spawn(async move {
                        toggle_windows(&app_handle).await;
                    });
                } else if event.id() == "back" {
                    if let Some(w) = get_active_window(app) {
                        let _ = w.eval("window.history.back()");
                    }
                } else if event.id() == "forward" {
                    if let Some(w) = get_active_window(app) {
                        let _ = w.eval("window.history.forward()");
                    }
                } else if event.id() == "reload" {
                    if let Some(w) = get_active_window(app) {
                        let _ = w.eval("window.location.reload()");
                    }
                } else if event.id() == "devtools" {
                    if let Some(w) = app.get_webview_window("music") { if w.is_visible().unwrap_or(false) { let _ = w.open_devtools(); } }
                    if let Some(w) = app.get_webview_window("rym") { if w.is_visible().unwrap_or(false) { let _ = w.open_devtools(); } }
                } else if event.id() == "quit" {
                    std::process::exit(0);
                }
            });

            // Initialize database
            // The database initialization and app.manage call have been moved to the beginning of the setup block.

            // Tab UI Injection Script
            let tab_ui_script = r#"
                (function() {
                    console.log("RYM-APPLE-MUSIC: Persistence Engine Starting...");
                    
                    const STYLE_ID = 'tauri-tabs-style';
                    const CONTAINER_ID = 'tauri-tabs';
                    const TOAST_ID = 'tauri-toast';
                    const IS_MUSIC = window.location.host.includes('apple.com');
                    const IS_RYM = window.location.host.includes('rateyourmusic.com');
                    
                    const getWords = (text) => {
                        if (!text) return [];
                        return text.toLowerCase()
                            .replace(/sir /g, '')
                            .replace(/the /g, '')
                            .replace(/orchestra/g, '')
                            .replace(/philharmonic/g, 'phil')
                            .replace(/philharmoniker/g, 'phil')
                            .replace(/berliner/g, 'berlin')
                            .replace(/wiener/g, 'vienna')
                            .split(/[^a-z0-9]+/)
                            .filter(w => w.length > 2); // Ignore short words like 'is', 'a', 'of'
                    };

                    const isArtistMatch = (artistA, artistB) => {
                        if (!artistA || !artistB) return false;
                        const a = artistA.toLowerCase();
                        const b = artistB.toLowerCase();
                        if (a.includes(b) || b.includes(a)) return true;
                        
                        const wordsA = getWords(a);
                        const wordsB = getWords(b);
                        
                        // Check for significant overlap (at least 2 words or 50% of shorter list)
                        const common = wordsA.filter(w => wordsB.includes(w));
                        const minRequired = Math.min(2, Math.max(1, Math.floor(Math.min(wordsA.length, wordsB.length) * 0.5)));
                        return common.length >= minRequired;
                    };

                    console.log(`RYM-APPLE-MUSIC: Persistence Engine Init. Host: ${window.location.host}, IS_MUSIC: ${IS_MUSIC}, IS_RYM: ${IS_RYM}`);
                    
                    window.showSyncToast = (msg) => {
                        console.log("RYM-APPLE-MUSIC: Toast requested:", msg);
                        const toast = document.getElementById(TOAST_ID);
                        if (toast) {
                            toast.textContent = msg;
                            toast.classList.add('show');
                            setTimeout(() => toast.classList.remove('show'), 4500);
                        } else {
                            console.warn("RYM-APPLE-MUSIC: Toast element not found yet, but requested:", msg);
                        }
                    };

                    function inject() {
                        if (!document.body) return;

                        // --- MANUAL DRAG TRIGGER (The Fix) ---
                        if (!window.hasManualDrag) {
                            window.hasManualDrag = true;
                            window.addEventListener('mousedown', (e) => {
                                const path = e.composedPath();
                                
                                // 1. Identify if we are clicking an interactive element
                                // Simplified: Only protect critical controls (buttons, inputs, progress bar)
                                const isInteractive = path.some(el => {
                                    if (!el.tagName) return false;
                                    const tag = el.tagName.toLowerCase();
                                    const role = el.getAttribute ? el.getAttribute('role') : null;
                                    const cls = typeof el.className === 'string' ? el.className : 
                                                (el.className && typeof el.className.baseVal === 'string' ? el.className.baseVal : "");
                                    const id = typeof el.id === 'string' ? el.id : "";

                                    return tag === 'a' || tag === 'button' || tag === 'input' || tag === 'select' || tag === 'textarea' ||
                                           tag === 'time' || tag === 'amp-lcd' ||
                                           role === 'button' || role === 'link' || role === 'slider' ||
                                           cls.includes('progress') || cls.includes('lcd') ||
                                           id.includes('progress') || id.includes('playback');
                                });

                                // 2. Identify if we are clicking a designated "drag region"
                                const isDragRegion = path.some(el => el.hasAttribute && el.hasAttribute('data-tauri-drag'));

                                if (isDragRegion && !isInteractive) {
                                    e.stopImmediatePropagation();
                                    e.preventDefault();
                                    
                                    const invoke = window.__TAURI__.core ? window.__TAURI__.core.invoke : window.__TAURI__.invoke;
                                    if (invoke) {
                                        invoke('start_drag').catch(err => console.error('RYM-APPLE-MUSIC: Drag failed:', err));
                                    }
                                }
                            }, true); 
                        }

                        // 1. ENSURE GLOBAL CSS IS INJECTED
                        if (!document.getElementById(STYLE_ID)) {
                            console.log("RYM-APPLE-MUSIC: Injecting Production Styles");
                            const style = document.createElement('style');
                            style.id = STYLE_ID;
                            style.textContent = `
                                [data-tauri-drag] { 
                                    -webkit-app-region: drag !important; 
                                    cursor: default !important; 
                                    z-index: 2147483645 !important;
                                }
                                [data-tauri-no-drag] { 
                                    -webkit-app-region: no-drag !important; 
                                    pointer-events: auto !important; 
                                    z-index: 2147483646 !important;
                                }
                                
                                
                                /* Make sure we can see these borders */
                                /* body { margin-top: 5px !important; } */

                                ${IS_MUSIC ? `
                                    .logo, .player-bar, amp-chrome-player { -webkit-app-region: drag !important; z-index: 2147483645 !important; }
                                    .logo a, .logo button, .player-bar button, .player-bar input, .player-bar a, .player-bar [role="button"], amp-lcd { 
                                        -webkit-app-region: no-drag !important; 
                                        z-index: 2147483646 !important;
                                    }
                                ` : ''}
                                
                                ${IS_RYM ? `
                                    header#page-header, header#page_header, .header, #page_header { 
                                        display: flex !important; 
                                        height: 54px !important; 
                                        -webkit-app-region: drag !important;
                                        z-index: 2147483640 !important;
                                    }
                                    .header_inner { 
                                        display: flex !important; 
                                        align-items: center !important; 
                                        justify-content: space-between !important;
                                        margin-left: 50px !important;
                                        width: calc(100% - 50px) !important;
                                        -webkit-app-region: drag !important;
                                    }
                                    /* Ensure segments inside are clickable */
                                    .header_inner > div, .header_inner > a, .header_inner > button, .header_inner input, #header_icon_link_bars {
                                        -webkit-app-region: no-drag !important;
                                        pointer-events: auto !important;
                                    }
                                ` : ''}

                                #tauri-tabs {
                                    position: fixed !important;
                                    bottom: 20px !important;
                                    left: 50% !important;
                                    transform: translateX(-50%) !important;
                                    z-index: 2147483647 !important;
                                    display: flex !important;
                                    gap: 8px !important;
                                    background: rgba(20, 20, 20, 0.6) !important;
                                    padding: 6px 12px !important;
                                    border-radius: 20px !important;
                                    border: 1px solid rgba(251, 35, 59, 0.5) !important;
                                    backdrop-filter: blur(15px) !important;
                                    box-shadow: 0 4px 15px rgba(0,0,0,0.5) !important;
                                    -webkit-app-region: no-drag !important;
                                    pointer-events: auto !important;
                                    opacity: 0.8 !important;
                                    transition: all 0.3s ease !important;
                                }
                                #tauri-tabs:hover { opacity: 1; transform: translateX(-50%) translateY(-2px); background: rgba(30,30,30,0.9); }
                                
                                #tauri-actions {
                                    position: fixed !important;
                                    bottom: 20px !important;
                                    right: 20px !important;
                                    z-index: 2147483647 !important;
                                    display: flex !important;
                                    gap: 8px !important;
                                    pointer-events: none !important;
                                }
                                #tauri-actions button {
                                    pointer-events: auto !important;
                                    background: rgba(20, 20, 20, 0.8) !important;
                                    backdrop-filter: blur(15px) !important;
                                    border: 1px solid rgba(251, 35, 59, 0.4) !important;
                                    color: white !important;
                                    padding: 8px 16px !important;
                                    border-radius: 20px !important;
                                    cursor: pointer !important;
                                    font-size: 12px !important;
                                    font-weight: 700 !important;
                                    box-shadow: 0 4px 15px rgba(0,0,0,0.4) !important;
                                    transition: all 0.2s ease !important;
                                }
                                #tauri-actions button:hover {
                                    transform: translateY(-2px);
                                    background: rgba(30, 30, 30, 0.9) !important;
                                    border-color: #fb233b !important;
                                }

                                .tauri-tab-btn {
                                    background: transparent !important;
                                    color: white !important;
                                    border: none !important;
                                    padding: 4px 12px !important;
                                    border-radius: 12px !important;
                                    cursor: pointer !important;
                                    font-size: 11px !important;
                                    font-weight: 700 !important;
                                    opacity: 0.5;
                                }
                                .tauri-tab-btn.active { background: #fb233b !important; opacity: 1; }
                                #tauri-toast {
                                    position: fixed !important;
                                    bottom: 110px !important;
                                    left: 50% !important;
                                    transform: translateX(-50%) !important;
                                    background: #fb233b !important;
                                    color: white !important;
                                    padding: 12px 24px !important;
                                    border-radius: 30px !important;
                                    z-index: 2147483647 !important;
                                    visibility: hidden; opacity: 0;
                                    transition: all 0.3s ease;
                                }
                                #tauri-toast.show { visibility: visible; opacity: 1; }
                            `;
                            (document.head || document.documentElement).appendChild(style);
                        }
                        // VIGOROUSLY UPDATE DRAG REGIONS (Precise Mode)
                        // Only mark the top-level containers. 
                        // The 'mousedown' listener will handle the check for "is this inside a drag region?"
                        const bars = Array.from(document.querySelectorAll('header, nav, [class*="player-bar"], [class*="header"], [id*="header"], amp-chrome-player, .logo, #page_header, .chrome-player'));
                        
                        bars.forEach(el => {
                            if (el.id === CONTAINER_ID) return;
                            
                            const style = window.getComputedStyle(el);
                            const isFixed = style.position === 'fixed' || style.position === 'sticky';
                            const rect = el.getBoundingClientRect();
                            const isAtEdge = isFixed && (rect.top <= 10 || (window.innerHeight - rect.bottom) <= 10);
                            const isHeaderType = /header|player|logo|nav/i.test(el.className + el.id + el.tagName);

                            if (isAtEdge || isHeaderType) {
                                // Just mark the container. Do not touch children.
                                if (!el.hasAttribute('data-tauri-drag')) {
                                    el.setAttribute('data-tauri-drag', '');
                                }
                                // Ensure the container itself has the cursor
                                if (el.style.cursor !== 'default') {
                                    el.style.cursor = 'default';
                                }
                            }
                        });


                        // TAG INTERACTIVE ELEMENTS AS NO-DRAG (Legacy CSS support)
                        // Even though we use manual drag, keeping this helps visual debugging if we ever re-enable borders
                        document.querySelectorAll('[data-tauri-drag] a, [data-tauri-drag] button, [data-tauri-drag] input, [data-tauri-drag] [role="button"], [data-tauri-drag] amp-lcd, [data-tauri-drag] amp-chrome-volume, [data-tauri-drag] amp-lcd-progress, [data-tauri-drag] .ui_search, [data-tauri-drag] .header_item').forEach(el => {
                            if (!el.hasAttribute('data-tauri-no-drag')) {
                                el.setAttribute('data-tauri-no-drag', '');
                            }
                        });
                        
                        // Cleanups
                        if (IS_MUSIC) {
                            const logoA = document.querySelector('.logo a');
                            if (logoA) logoA.remove();
                        }
                        
                        if (IS_RYM) {
                            const h = document.getElementById('page_header') || document.getElementById('page-header');
                            if (h && h.style.display !== 'flex') {
                                h.style.display = 'flex';
                                h.style.height = '54px';
                            }
                        }

                        if (document.getElementById(CONTAINER_ID)) return;
                        
                        console.log("RYM-APPLE-MUSIC: Injecting Container");
                        if (!document.getElementById(TOAST_ID)) {
                            const toast = document.createElement('div');
                            toast.id = TOAST_ID;
                            document.body.appendChild(toast);
                        }

                        const container = document.createElement('div');
                        container.id = CONTAINER_ID;
                        
                        const isMusic = IS_MUSIC;
                         
                        
                        const musicBtn = document.createElement('button');
                        musicBtn.className = 'tauri-tab-btn' + (isMusic ? ' active' : '');
                        musicBtn.textContent = 'Music';
                        musicBtn.onclick = () => {
                            window.__TAURI__.core.invoke('show_music').catch(err => {
                                console.error('RYM-APPLE-MUSIC: Failed to show music window:', err);
                                // Fallback to a direct focus if possible
                                if (err.toString().includes('not allowed')) {
                                    window.showSyncToast('Permission Error: Restart App');
                                }
                            });
                        };
                        
                        const rymBtn = document.createElement('button');
                        rymBtn.className = 'tauri-tab-btn' + (!isMusic ? ' active' : '');
                        rymBtn.textContent = 'RYM';
                        rymBtn.onclick = () => {
                            window.__TAURI__.core.invoke('show_rym').catch(err => {
                                console.error('RYM-APPLE-MUSIC: Failed to show RYM window:', err);
                            });
                        };
                        
                        container.appendChild(musicBtn);
                        container.appendChild(rymBtn);

                        // SEPARATOR
                        const sep = document.createElement('div');
                        sep.style.width = '1px';
                        sep.style.height = '16px';
                        sep.style.background = 'rgba(255,255,255,0.2)';
                        sep.style.margin = '0 4px';
                        container.appendChild(sep);

                        (document.body || document.documentElement).appendChild(container);
                        
                        // ACTION BUTTON CONTAINER (Detached)
                        if (!document.getElementById('tauri-actions')) {
                            const actionContainer = document.createElement('div');
                            actionContainer.id = 'tauri-actions';
                            (document.body || document.documentElement).appendChild(actionContainer);
                        }

                        // --- MATCH STATE MACHINE ---
                        function updateMatchState() {
                            const actionBox = document.getElementById('tauri-actions');
                            if (!actionBox || !IS_RYM) return;

                            const lastSynced = localStorage.getItem('tauri_last_synced_album');
                            const syncOccurred = localStorage.getItem('tauri_sync_occurred') === 'true';
                            const ddgActive = localStorage.getItem('tauri_ddg_sync_active') === 'true';
                            const isReleasePage = window.location.pathname.includes('/release/');
                            
                            // 1. Handle DDG Redirect Result
                            if (ddgActive) {
                                console.log("RYM-APPLE-MUSIC: DDG Sync detected. Clearing flag.");
                                localStorage.removeItem('tauri_ddg_sync_active');
                                
                                if (!isReleasePage) {
                                    window.showSyncToast('No direct match. Search or find manually.');
                                } else {
                                    window.showSyncToast('Synced (Auto-Match)');
                                }
                            }

                            // 2. Render Buttons based on State
                            const correctionMode = localStorage.getItem('tauri_correction_mode') === 'true';
                            
                            // Clear current buttons
                            actionBox.innerHTML = '';

                            if (lastSynced && syncOccurred) {
                                if (correctionMode) {
                                    // STATE: CORRECTION MODE (User is finding the right album)
                                    const setMatchBtn = document.createElement('button');
                                    setMatchBtn.textContent = 'Set Match';
                                    
                                    if (!isReleasePage) {
                                        setMatchBtn.style.opacity = '0.5';
                                        setMatchBtn.disabled = true;
                                        setMatchBtn.title = "Navigate to a release page first";
                                    } else {
                                        setMatchBtn.style.cursor = 'pointer';
                                        setMatchBtn.title = `Link current page to: ${lastSynced}`;
                                        setMatchBtn.onclick = () => {
                                             if (confirm(`Link this RYM page to Apple Music album:\n"${lastSynced}"?`)) {
                                                 const parts = lastSynced.split(' - ');
                                                 if (parts.length >= 1) {
                                                     let targetArtist = parts[0];
                                                     let targetAlbum = parts.slice(1).join(' - ');
                                                     if (!targetAlbum) targetAlbum = "Unknown Album";
                                                     
                                                     const rating = document.querySelector('.avg_rating')?.innerText?.trim();
                                                     const count = document.querySelector('.num_ratings b')?.innerText?.trim();
                                                     const genreLinks = document.querySelectorAll('.release_pri_genres a');
                                                     const genres = Array.from(genreLinks).map(a => a.innerText).join(', ');
                                                     
                                                     let date = "";
                                                     const rows = Array.from(document.querySelectorAll('.album_info tr'));
                                                     const releasedRow = rows.find(r => r.querySelector('.info_hdr')?.innerText?.includes('Released'));
                                                     if (releasedRow) date = releasedRow.querySelector('td')?.innerText?.trim() || "";
                                                     
                                                     const titleEl = document.querySelector('.album_title');
                                                     let rymAlbum = "";
                                                     if (titleEl) {
                                                         const clone = titleEl.cloneNode(true);
                                                         clone.querySelectorAll('.year, .release_year, .album_shortcut, .album_artist_small').forEach(el => el.remove());
                                                         rymAlbum = clone.innerText?.trim() || "";
                                                     }
                                                     const rymArtist = document.querySelector('.album_info .artist')?.innerText?.trim() || "";
                                                     
                                                     if (rating) {
                                                         window.__TAURI__.core.invoke('set_manual_match', {
                                                             targetArtist: targetArtist,
                                                             targetAlbum: targetAlbum,
                                                             rating: {
                                                                album_name: rymAlbum,
                                                                artist_name: rymArtist,
                                                                rym_rating: parseFloat(rating),
                                                                rating_count: parseInt(count?.replace(/,/g, '') || "0"),
                                                                rym_url: window.location.href,
                                                                genres: genres,
                                                                release_date: date || "",
                                                                timestamp: Date.now()
                                                             }
                                                         });
                                                         window.showSyncToast('Link Saved!');
                                                         localStorage.removeItem('tauri_correction_mode');
                                                         localStorage.removeItem('tauri_sync_occurred');
                                                         updateMatchState();
                                                     } else {
                                                         window.showSyncToast('Error: No rating data found');
                                                     }
                                                 }
                                             }
                                        };
                                    }
                                    actionBox.appendChild(setMatchBtn);

                                    // NO MATCH BUTTON
                                    const noMatchBtn = document.createElement('button');
                                    noMatchBtn.textContent = 'No Match';
                                    noMatchBtn.onclick = () => {
                                        if (confirm(`Mark "${lastSynced}" as having NO match on RYM?`)) {
                                             const parts = lastSynced.split(' - ');
                                             if (parts.length >= 1) {
                                                 let targetArtist = parts[0];
                                                 let targetAlbum = parts.slice(1).join(' - ') || "Unknown Album";
                                                 
                                                 window.__TAURI__.core.invoke('set_manual_match', {
                                                     targetArtist: targetArtist,
                                                     targetAlbum: targetAlbum,
                                                     rating: {
                                                        album_name: "NO_MATCH",
                                                        artist_name: "NO_MATCH",
                                                        rym_rating: 0.0,
                                                        rating_count: 0,
                                                        rym_url: "NO_MATCH",
                                                        genres: "",
                                                        release_date: "",
                                                        timestamp: Date.now()
                                                     }
                                                 });
                                                 window.showSyncToast('Marked as No Match');
                                                 localStorage.removeItem('tauri_correction_mode');
                                                 localStorage.removeItem('tauri_sync_occurred');
                                                 updateMatchState();
                                             }
                                        }
                                    };
                                    actionBox.appendChild(noMatchBtn);

                                } else {
                                    // STATE: SYNCED (Show "Wrong Match?")
                                    // Only show if we are on a release page (or the page we landed on)
                                    const wrongMatchBtn = document.createElement('button');
                                    wrongMatchBtn.textContent = 'Wrong Match?';
                                    wrongMatchBtn.onclick = () => {
                                        localStorage.setItem('tauri_correction_mode', 'true');
                                        updateMatchState();
                                        window.showSyncToast('Find the correct album, then click Set Match');
                                    };
                                    actionBox.appendChild(wrongMatchBtn);
                                }
                            }
                        }

                        // Run state check periodically
                        setInterval(updateMatchState, 1000);
                        setTimeout(updateMatchState, 500); // Initial check
                    }

                    let lastLoggedInfo = "";
                    let lastScrapedUrl = "";
                    window.extractMusicInfo = () => {
                        let strategy = "";
                        // Strategy 1: Album Page
                        let album = document.querySelector('.headings__title span[dir="auto"]')?.innerText || 
                                    document.querySelector('[data-testid="non-editable-product-title"] span')?.innerText;
                        
                        let artistElements = document.querySelectorAll('.headings__subtitles a, [data-testid="product-subtitles"] a');
                        let artists = Array.from(artistElements).map(a => a.innerText.trim().replace(/^Sir /i, ''));
                        let artist = artists.join(' ');

                        if (artist || album) strategy = "Album Page";

                        // Strategy 2: Shadow DOM LCD (Now Playing)
                        if (!artist || !album) {
                            const lcd = document.querySelector('amp-lcd');
                            // Specifically target the secondary line which contains Artist - Album
                            const lcdContent = lcd?.shadowRoot?.querySelector('.lcd-meta__secondary .lcd-meta-line__text-content');
                            if (lcdContent) {
                                const text = lcdContent.textContent || "";
                                // Handle different types of dashes (em-dash, en-dash, hyphen)
                                const separator = text.includes(' — ') ? ' — ' : (text.includes(' - ') ? ' - ' : (text.includes(' – ') ? ' – ' : null));
                                
                                if (separator) {
                                    const parts = text.split(separator);
                                    if (parts.length >= 2) {
                                        artist = parts[0].trim().replace(/^Sir /i, '');
                                        album = parts[1].trim();
                                        strategy = "Shadow LCD";
                                    }
                                }
                            }
                        }

                        // Strategy 3: Playback Bar (LCD) - Look for specific AM fragments (Fallback)
                        if (!artist || !album) {
                            artist = document.querySelector('.lcd-meta__secondary .lcd-meta-line__fragment:nth-child(1)')?.innerText;
                            album = document.querySelector('.lcd-meta__secondary .lcd-meta-line__fragment:nth-child(3)')?.innerText;
                            if (artist || album) strategy = "Playback Bar Fallback";
                        }

                        if (artist && artist.trim() && album && album.trim()) {
                            const infoKey = `${artist.trim()} - ${album.trim()}`;
                            if (infoKey !== lastLoggedInfo) {
                                console.log(`RYM-APPLE-MUSIC: Extracted via ${strategy} - ${infoKey}`);
                                lastLoggedInfo = infoKey;
                            }
                            return { artist: artist.trim(), album: album.trim() };
                        }
                        
                        if (window.location.host.includes('music.apple.com') && window.location.pathname.includes('/album/')) {
                            if (lastLoggedInfo !== "FAIL") {
                                console.warn("RYM-APPLE-MUSIC: On album page but extraction failed. DOM mismatch?");
                                lastLoggedInfo = "FAIL";
                            }
                        }
                        return null;
                    };

                    window.syncToRym = (background = false) => {
                        const info = window.extractMusicInfo();
                        if (info) {
                            window.__TAURI__.core.invoke('sync_to_rym', { 
                                artist: info.artist, 
                                album: info.album,
                                background: background,
                                musicUrl: window.location.href
                            });
                        } else if (!background) {
                            window.showSyncToast('Navigate to an album or play something!');
                        }
                    };

                    function checkAutoSync() {
                        if (!IS_MUSIC) return;
                         
                        
                        const info = window.extractMusicInfo();
                        if (info) {
                            const albumKey = `${info.artist} - ${info.album}`;
                            const lastSynced = localStorage.getItem('tauri_last_synced_album');
                            const ignoreFlag = localStorage.getItem('tauri_ignore_next_sync');
                            
                            if (ignoreFlag === 'true') {
                                console.log("RYM-APPLE-MUSIC: Sync loop prevented (Ignore flag set). Clearing flag.");
                                localStorage.removeItem('tauri_ignore_next_sync');
                                localStorage.setItem('tauri_last_synced_album', albumKey);
                                
                                // FORCE A SYNC (FETCH ONLY) to populate UI
                                console.log("RYM-APPLE-MUSIC: Force triggering sync to fetch metadata (without navigation).");
                                window.syncToRym(true);
                                if (window.sessionRequestedAlbums) window.sessionRequestedAlbums.add(albumKey);
                                return;
                            }

                            // Check if we need to sync:
                            // 1. New album detected (classic check)
                            // 2. OR we haven't requested data for this album in this session (Refresh Fix)
                            const sessionRequested = window.sessionRequestedAlbums && window.sessionRequestedAlbums.has(albumKey);
                            
                            if (albumKey !== lastSynced || !sessionRequested) {
                                if (albumKey !== lastSynced) {
                                    console.log("RYM-APPLE-MUSIC: New album detected. Triggering auto-sync:", albumKey);
                                    // CLEAR OLD METADATA IMMEDIATELY (Stale Data Fix)
                                    const existing = document.getElementById('rym-injected-meta');
                                    if (existing) {
                                        console.log("RYM-APPLE-MUSIC: Clearing stale metadata for new album.");
                                        existing.remove();
                                        const target = document.querySelector('.headings__metadata-bottom');
                                        if (target) target.style.display = '';
                                    }
                                    window.lastRymMetadata = null; // Ensure stale data is gone from memory
                                } else {
                                    console.log("RYM-APPLE-MUSIC: Refresh detected (Session missing). Re-fetching:", albumKey);
                                }
                                
                                localStorage.setItem('tauri_last_synced_album', albumKey);
                                if (window.sessionRequestedAlbums) window.sessionRequestedAlbums.add(albumKey);
                                
                                window.syncToRym(true); // Trigger background sync
                            }
                        } else {
                            // If we fail to extract but we are on an album page, log why occasionally
                            if (window.location.pathname.includes('/album/') && Math.random() < 0.05) {
                                console.log("RYM-APPLE-MUSIC: checkAutoSync - No info extracted on album page.");
                            }
                        }
                    }

                    window.addEventListener('keydown', (e) => {
                        if ((e.metaKey || e.ctrlKey) && e.key === 'f') {
                            if (IS_MUSIC) {
                                const searchInput = document.querySelector('input.search-input__text-field');
                                if (searchInput) {
                                    e.preventDefault();
                                    searchInput.focus();
                                    searchInput.select();
                                }
                            } else {
                                const searchInput = document.getElementById('ui_search_input_main_search');
                                if (searchInput) {
                                    e.preventDefault();
                                    searchInput.focus();
                                    searchInput.select();
                                }
                            }
                        }
                    });

                    // Search Results Clicker Logic
                    if (window.location.host.includes('rateyourmusic.com') && 
                        window.location.pathname.includes('/search') && 
                        window.location.search.includes('sync=1')) {
                        
                        const params = new URLSearchParams(window.location.search);
                        const searchTerm = params.get('searchterm') || "";
                        
                        const getWords = (text) => {
                            if (!text) return [];
                            return text.toLowerCase()
                                .replace(/sir /g, '')
                                .replace(/the /g, '')
                                .replace(/orchestra/g, '')
                                .replace(/philharmonic/g, 'phil')
                                .replace(/philharmoniker/g, 'phil')
                                .split(/[^a-z0-9]+/)
                                .filter(w => w.length > 2); // Ignore short words like 'is', 'a', 'of'
                        };

                        const targetWords = getWords(searchTerm);
                        console.log(`RYM-APPLE-MUSIC: SEARCH DEBUG - Target: "${searchTerm}"`);
                        console.log(`RYM-APPLE-MUSIC: SEARCH DEBUG - Target Words: [${targetWords.join(', ')}]`);

                        let attempts = 0;
                        const interval = setInterval(() => {
                            attempts++;
                            
                            const rows = Array.from(document.querySelectorAll('tr.infobox'));
                            if (rows.length === 0) {
                                if (attempts % 5 === 0) console.log("RYM-APPLE-MUSIC: Waiting for search results to load...");
                                return;
                            }

                            let bestMatch = null;
                            let maxScore = 0;
                            let bestMatchedWords = [];

                            rows.forEach((row, index) => {
                                const releaseLink = row.querySelector('a.searchpage');
                                if (!releaseLink || !releaseLink.href.includes('/release/')) return;
                                
                                const artistLinks = Array.from(row.querySelectorAll('a.artist'));
                                const artistNames = artistLinks.map(a => a.innerText).join(' ');
                                const releaseTitle = releaseLink.innerText;
                                
                                const resultWords = getWords(artistNames + " " + releaseTitle);
                                
                                // Calculate score: how many target words are present in this result?
                                let matchedWords = [];
                                let score = 0;
                                
                                targetWords.forEach(tw => {
                                    if (resultWords.some(rw => rw.includes(tw) || tw.includes(rw))) {
                                        score += 2;
                                        matchedWords.push(tw);
                                    }
                                });

                                // Log details for every result on the first attempt (or if it's potentially a match)
                                if (attempts === 1 || score > 4) {
                                    console.log(`RYM-APPLE-MUSIC: Result #${index} [Score ${score}]: "${artistNames} - ${releaseTitle}"`);
                                    if (score > 0) {
                                        console.log(`   Matches: [${matchedWords.join(', ')}]`);
                                    }
                                }

                                if (score > maxScore) {
                                    maxScore = score;
                                    bestMatch = releaseLink;
                                    bestMatchedWords = matchedWords;
                                }
                            });
                            
                            // Checking for matches
                            if (bestMatch) {
                                // Calculate how many unique target words covered
                                const uniqueMatches = new Set(bestMatchedWords).size;
                                const uniqueTargets = new Set(targetWords).size;
                                const matchRatio = uniqueMatches / uniqueTargets;

                                // CONDITION 1: Perfect Match (All target words found)
                                // We trust this implicitly.
                                const isPerfectMatch = matchRatio >= 1.0;

                                // CONDITION 2: Strong Match (Score >= 4, meaning at least 2 distinct words matched)
                                // AND it's one of the top results (index 0 or 1)
                                const isStrongMatch = maxScore >= 4;

                                if (isPerfectMatch || (isStrongMatch && attempts >= 2)) {
                                    console.log(`RYM-APPLE-MUSIC: Best candidate selected (Perfect: ${isPerfectMatch}, Score: ${maxScore}):`, bestMatch.innerText);
                                    clearInterval(interval);
                                    window.showSyncToast('Matched: ' + bestMatch.innerText);
                                    bestMatch.click();
                                    return;
                                }
                            }
                            
                            if (attempts > 8) {
                                console.warn("RYM-APPLE-MUSIC: Match timeout. No strong match found.");
                                window.showSyncToast('No auto-match found. Please select manually.');
                                clearInterval(interval);
                            }
                        }, 500);
                    }

                    function syncLink() {
                        if (!document.body) return;
                        // Only sync if we are on an album page
                        if (IS_RYM && 
                            window.location.pathname.includes('/release/')) { // Changed to /release/ to cover all release types
                            
                            const ignoreFlag = localStorage.getItem('tauri_ignore_next_sync');
                            if (ignoreFlag === 'true') {
                                console.log("RYM-APPLE-MUSIC: Sync loop prevented (Ignore flag set on RYM). Clearing flag.");
                                localStorage.removeItem('tauri_ignore_next_sync');
                                // Also update last pending URL to prevent immediate re-sync
                                const allAmLinks = Array.from(document.body.querySelectorAll('a[href*="music.apple.com"]'));
                                const amLink = allAmLinks.find(a => a.href.includes('/album/')) || allAmLinks[0];
                                if (amLink) localStorage.setItem('tauri_last_pending_url', amLink.href);
                                return;
                            }

                            // Only trigger sync if focused (manual-ish) or if it's a truly new discovery
                            if (!document.hasFocus()) return;

                            const allAmLinks = Array.from(document.body.querySelectorAll('a[href*="music.apple.com"]'));
                            
                            // Prioritize album links, then any AM link
                            const amLink = allAmLinks.find(a => a.href.includes('/album/')) || allAmLinks[0];
                            
                            if (amLink && amLink.href) {
                                const lastUrl = localStorage.getItem('tauri_last_pending_url');
                                if (amLink.href !== lastUrl) {
                                    console.log("RYM-APPLE-MUSIC: Syncing Apple Music link:", amLink.href);
                                    localStorage.setItem('tauri_last_pending_url', amLink.href);
                                    
                                    // Extract Metadata to send for verification
                                    let rymArtist = document.querySelector('.album_info .artist')?.innerText?.trim() || null;
                                    let rymAlbum = null;
                                    const titleEl = document.querySelector('.album_title');
                                    if (titleEl) {
                                        const clone = titleEl.cloneNode(true);
                                        clone.querySelectorAll('.year, .release_year, .album_shortcut, .album_artist_small').forEach(el => el.remove());
                                        rymAlbum = clone.innerText?.trim() || null;
                                    }
                                    
                                    window.__TAURI__.core.invoke('set_pending_music_url', { 
                                        url: amLink.href,
                                        artist: rymArtist,
                                        album: rymAlbum
                                    });
                                }
                            }
                        }
                    }

                    function sampleHtml() {
                        window.sampledPageTypes = window.sampledPageTypes || {};
                        if (!IS_RYM) return;
                        
                        // Relaxed Wait: Check for footer (signals main layout) OR just wait for non-empty content
                        const hasFooter = !!document.querySelector('#footer, .footer, #footer_copy');
                        const hasContent = document.body && document.body.innerText.trim().length > 200;
                        
                        if (!hasContent && !hasFooter) return;

                        const segments = window.location.pathname.split('/').filter(s => s.length > 0);
                        let type = "homepage";
                        if (segments.length > 0) {
                            type = segments[0];
                            if (type === 'release' && segments.length > 1) {
                                type = 'release_' + segments[1];
                            }
                        }

                        if (window.sampledPageTypes[type]) return;

                        window.sampledPageTypes[type] = true;
                        console.log("RYM-APPLE-MUSIC: Sampling new type (Fully Loaded): " + type);
                        const bodyHtml = document.body.outerHTML;
                        window.__TAURI__.core.invoke('save_sample_html', { 
                            pageType: type, 
                            url: window.location.href,
                            html: bodyHtml 
                        });
                    }

                    let observedShadowRoot = null;
                    function setupShadowObserver() {
                        const lcd = document.querySelector('amp-lcd');
                        if (lcd && lcd.shadowRoot && observedShadowRoot !== lcd.shadowRoot) {
                            console.log("RYM-APPLE-MUSIC: Attaching observer to Shadow DOM");
                            const shadowObserver = new MutationObserver(() => {
                                checkAutoSync();
                            });
                            shadowObserver.observe(lcd.shadowRoot, { childList: true, subtree: true, characterData: true });
                            observedShadowRoot = lcd.shadowRoot;
                        }
                    }

                    function handleRymMetadata() {
                        const path = window.location.pathname;
                        // Updated to match any release type
                        const isRelease = path.startsWith('/release/'); 
                        if (!IS_RYM || !isRelease) return;

                        if (window.location.href === lastScrapedUrl) return;

                        const rating = document.querySelector('.avg_rating')?.innerText?.trim();
                        // Wait until rating is present before sending
                        if (!rating) return;

                        lastScrapedUrl = window.location.href;
                        console.log("RYM-APPLE-MUSIC: Starting metadata extraction...");
                        const count = document.querySelector('.num_ratings b')?.innerText?.trim();
                        
                        // Robust Date Extraction
                        let date = "";
                        const rows = Array.from(document.querySelectorAll('.album_info tr'));
                        const releasedRow = rows.find(r => r.querySelector('.info_hdr')?.innerText?.includes('Released'));
                        if (releasedRow) {
                            date = releasedRow.querySelector('td')?.innerText?.trim() || "";
                            console.log("RYM-APPLE-MUSIC: Extracted date via row search:", date);
                        }
                        
                        // Fallback to XPath if needed
                        if (!date) {
                            date = document.evaluate("//th[contains(text(),'Released')]/following-sibling::td", document, null, XPathResult.FIRST_ORDERED_NODE_TYPE, null).singleNodeValue?.innerText?.trim() || "";
                            if (date) console.log("RYM-APPLE-MUSIC: Extracted date via XPath fallback:", date);
                        }

                        const genreLinks = document.querySelectorAll('.release_pri_genres a');
                        const genres = Array.from(genreLinks).map(a => a.innerText).join(', ');
                        
                        // Extract Album and Artist from RYM page
                        const titleEl = document.querySelector('.album_title');
                        let rymAlbum = "";
                        if (titleEl) {
                            const clone = titleEl.cloneNode(true);
                            clone.querySelectorAll('.year, .release_year, .album_shortcut, .album_artist_small').forEach(el => el.remove());
                            rymAlbum = clone.innerText?.trim() || "";
                        }
                        const rymArtist = document.querySelector('.album_info .artist')?.innerText?.trim() || "";
                        
                        console.log(`RYM-APPLE-MUSIC: Found ${genreLinks.length} primary genres:`, genres);
                        console.log(`RYM-APPLE-MUSIC: Extracted RYM - Artist: ${rymArtist}, Album: ${rymAlbum}`);

                        if (rating) {
                            console.log("RYM-APPLE-MUSIC: Extraction complete. Sending to Rust:", { rating, count, date, genres, rymAlbum, rymArtist });
                            window.__TAURI__.core.invoke('save_rym_rating', { 
                                rating: {
                                    album_name: rymAlbum,
                                    artist_name: rymArtist,
                                    rym_rating: parseFloat(rating),
                                    rating_count: parseInt(count?.replace(/,/g, '') || "0"),
                                    rym_url: window.location.href,
                                    genres: genres,
                                    release_date: date || "",
                                    timestamp: Date.now()
                                }
                            });
                        } else {
                            console.warn("RYM-APPLE-MUSIC: Extraction failed - No rating found on page.");
                        }
                    }

                    // Listen for metadata events to inject into AM
                    if (IS_MUSIC) {
                        window.lastRymMetadata = null;
                        window.sessionRequestedAlbums = new Set(); // Track requests this session

                        function injectMetadata(data) {
                            if (data.rym_url === 'NO_MATCH') return false;

                            const target = document.querySelector('.headings__metadata-bottom');
                            if (!target) return false;
                            
                            // Hide the original metadata
                            target.style.display = 'none';

                            // Remove old injected one if exists
                            const existing = document.getElementById('rym-injected-meta');
                            if (existing) existing.remove();

                            const metaDiv = document.createElement('div');
                            metaDiv.id = 'rym-injected-meta';
                            metaDiv.style.marginTop = '4px';
                            metaDiv.style.fontSize = '13px';
                            metaDiv.style.lineHeight = '1.6';
                            metaDiv.style.color = 'var(--labelSecondary, inherit)';
                            metaDiv.style.fontWeight = '400';

                            // Format: <rating> • <release date>\n<genres>
                            // Using toFixed(2) for consistency with RYM
                            metaDiv.innerHTML = `
                                <div>
                                    <span style="font-weight: 700;">${data.rym_rating.toFixed(2)}</span>
                                    <span style="margin: 0 4px; opacity: 0.5;">•</span>
                                    <span>${data.release_date}</span>
                                </div>
                                <div style="opacity: 0.7; font-size: 12px; margin-top: 2px;">${data.genres}</div>
                            `;
                            
                            target.parentNode.insertBefore(metaDiv, target);
                            console.log("RYM-APPLE-MUSIC: Successfully replaced metadata with RYM Insights.");
                            return true;
                        }

                        function checkAndReinject() {
                            const info = window.extractMusicInfo();
                            if (!info) return;

                            // If we have metadata, verify it matches current album
                            if (window.lastRymMetadata) {
                                const currentArtist = info.artist.toLowerCase();
                                const metaArtist = window.lastRymMetadata.artist_name.toLowerCase();
                                
                                const currentAlbum = info.album.toLowerCase();
                                const metaAlbum = window.lastRymMetadata.album_name.toLowerCase();

                                // Fuzzy check for artist AND album
                                const artistMatch = isArtistMatch(currentArtist, metaArtist);
                                const albumMatch = currentAlbum.includes(metaAlbum) || metaAlbum.includes(currentAlbum);

                                if (artistMatch && albumMatch) {
                                    if (!document.getElementById('rym-injected-meta')) {
                                        console.log("RYM-APPLE-MUSIC: Re-injecting missing metadata for current album.");
                                        injectMetadata(window.lastRymMetadata);
                                    }
                                } else {
                                    // NO MATCH - Clear immediately (Stale Data Fix)
                                    const existing = document.getElementById('rym-injected-meta');
                                    if (existing) {
                                        console.log("RYM-APPLE-MUSIC: Clearing stale metadata (Mismatch).");
                                        existing.remove();
                                        const target = document.querySelector('.headings__metadata-bottom');
                                        if (target) target.style.display = '';
                                    }
                                }
                            }
                        }

                        // Clear on manual URL changes too
                        let lastPath = window.location.pathname;
                        setInterval(() => {
                                if (window.location.pathname !== lastPath) {
                                    lastPath = window.location.pathname;
                                    const existing = document.getElementById('rym-injected-meta');
                                    if (existing) {
                                        existing.remove();
                                        const target = document.querySelector('.headings__metadata-bottom');
                                        if (target) target.style.display = '';
                                    }
                                    checkAndReinject();
                                }
                        }, 500);

                        window.__TAURI__.event.listen('rym-rating-updated', (event) => {
                            const data = event.payload;
                            console.log("RYM-APPLE-MUSIC: Received metadata for injection:", data);
                            data.receivedAt = Date.now(); // Mark receipt time
                            window.lastRymMetadata = data;
                            // Inject if it's broadly the same artist (safe fuzzy match)
                            const info = window.extractMusicInfo();
                            if (info) {
                                if (isArtistMatch(info.artist, data.artist_name)) {
                                    injectMetadata(data);
                                } else {
                                    console.log(`RYM-APPLE-MUSIC: Artist mismatch (${info.artist.toLowerCase()} vs ${data.artist_name.toLowerCase()}), not injecting.`);
                                }
                            } else {
                                injectMetadata(data); // Fallback to injecting if extraction fails (might be a slow load)
                            }
                        });
                    }

                    function checkDDGFail() {
                        if (!window.location.host.includes('duckduckgo.com')) return;
                        
                        const url = window.location.href;
                        const hasSiteFilter = url.includes('site%3Arateyourmusic.com');
                        // DDG Lucky URL starts with %5C (\). If it's missing but site filter is present, it's failed and landed on results.
                        const isFailedLucky = hasSiteFilter && !url.includes('q=%5C') && !url.includes('q=%255C');
                        
                        if (isFailedLucky) {
                            console.log("RYM-APPLE-MUSIC: DDG Lucky search failed (Redirect diverted). Falling back to RYM search...");
                            
                            const params = new URLSearchParams(window.location.search);
                            let query = params.get('q') || "";
                            
                            if (!query) {
                                const input = document.getElementById('search_form_input');
                                query = input ? input.value : "";
                            }

                            if (query) {
                                // Clean up the query: remove site filters and backslashes
                                let cleanQuery = query
                                    .replace(/\\/g, '')
                                    .replace(/site:rateyourmusic\.com\/release/i, '')
                                    .replace(/site:rateyourmusic\.com/i, '')
                                    .trim();
                                
                                if (cleanQuery) {
                                    const rymUrl = `https://rateyourmusic.com/search?searchterm=${encodeURIComponent(cleanQuery)}&searchtype=l&sync=1`;
                                    console.log("RYM-APPLE-MUSIC: Redirecting to RYM search:", rymUrl);
                                    window.location.href = rymUrl;
                                }
                            }
                        }
                    }

                    inject();
                    syncLink();
                    sampleHtml();
                    setupShadowObserver();
                    handleRymMetadata();
                    checkDDGFail();
                    
                    setInterval(() => {
                        inject();
                        syncLink();
                        sampleHtml();
                        checkAutoSync();
                        setupShadowObserver();
                        handleRymMetadata();
                        checkDDGFail();
                        if (window.location.host.includes('music.apple.com')) {
                            checkAndReinject(); // Check for re-injection on AM pages
                        }
                    }, 1000);
                    
                    const observer = new MutationObserver(() => {
                        inject();
                        syncLink();
                        sampleHtml();
                        checkAutoSync();
                        setupShadowObserver();
                        checkDDGFail();
                        if (window.location.host.includes('music.apple.com')) {
                            checkAndReinject();
                        }
                    });
                    
                    const obsTarget = document.body || document.documentElement;
                    observer.observe(obsTarget, { childList: true, subtree: true });
                })();
            "#;

            let window_size = tauri::LogicalSize::new(1600.0, 1000.0);

            // RESTORE APP
            let music_window = tauri::WebviewWindowBuilder::new(app, "music", tauri::WebviewUrl::External("https://music.apple.com".parse().unwrap()))
                .title("RYM Apple Music Player")
                .inner_size(1200.0, 800.0)
                .center()
                .visible(true)
                .hidden_title(true)
                .resizable(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .background_color(Color(20, 20, 26, 255)) // #14141a
                .devtools(true)
                .initialization_script(tab_ui_script)
                .build()
                .expect("Failed to create music window");

            // Create RYM window - Start with blank page to avoid loading RYM on launch
            // Will load RYM homepage only when user switches to RYM tab for the first time
            let rym_window = tauri::WebviewWindowBuilder::new(app, "rym", tauri::WebviewUrl::External("about:blank".parse().unwrap()))
                .title("RYM Apple Music Player")
                .inner_size(1200.0, 800.0)
                .center()
                .visible(false)
                .hidden_title(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .background_color(Color(20, 20, 26, 255)) // #14141a
                .devtools(true)
                .initialization_script(tab_ui_script)
                .build()
                .expect("Failed to create RYM window");
            
            // Auto-open devtools for debugging
            music_window.open_devtools();
            rym_window.open_devtools();
            
            // Still keep the hidden scraper window if needed for background tasks, 
            // but we can also just use the "rym" window for it.
            // For now, let's keep the existing architecture for scraping if it was working.
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_rym_rating, save_rym_rating, show_music, show_rym, set_pending_music_url, sync_to_rym, go_back, go_forward, save_sample_html, start_drag, set_manual_match])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
