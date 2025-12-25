mod database;
mod release_date;
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
    use release_date::{parse_release_date_to_timestamp, compute_ttl_seconds, is_fresh};
    
    println!("RYM-GET-RATING: ========================================");
    println!("RYM-GET-RATING: Request for: {} - {}", artist, album);
    
    let now = chrono::Utc::now().timestamp();
    
    // 1. Check SQLite
    println!("RYM-GET-RATING: Checking local SQLite cache...");
    let local_rating = {
        let db = state.db.lock().unwrap();
        db.get_rating(&album, &artist).ok().flatten()
    };
    
    let mut best_candidate: Option<AlbumRating> = None;
    
    if let Some(mut rating) = local_rating {
        let release_ts = parse_release_date_to_timestamp(&rating.release_date);
        let ttl = compute_ttl_seconds(now, release_ts);
        
        let tracks_json = rating.track_ratings.as_ref().map(|s| s.as_str()).unwrap_or("[]");
        let has_tracks = tracks_json.len() > 5;
        let reviews_json = rating.reviews.as_ref().map(|s| s.as_str()).unwrap_or("[]");
        let has_reviews = reviews_json.len() > 5;
        
        println!("RYM-GET-RATING: Found local cache entry.");
        println!("RYM-GET-RATING:   - Last fetched: {} ({} seconds ago)", rating.timestamp, now - rating.timestamp);
        println!("RYM-GET-RATING:   - TTL: {} seconds", ttl);
        println!("RYM-GET-RATING:   - Has tracks: {} (JSON len: {})", has_tracks, tracks_json.len());
        println!("RYM-GET-RATING:   - Has reviews: {} (JSON len: {})", has_reviews, reviews_json.len());
        
        if is_fresh(rating.timestamp, ttl, now) && has_tracks {
            println!("RYM-GET-RATING: ✓ LOCAL CACHE HIT (FRESH & COMPLETE)");
            rating.status = Some("fresh".to_string());
            return Ok(Some(rating));
        } else {
            let reason = if !has_tracks { "INCOMPLETE DATA (No tracks)" } else { "STALE (TTL expired)" };
            println!("RYM-GET-RATING: ⚠️ LOCAL CACHE HIT (RE-FETCH NEEDED - Reason: {})", reason);
            rating.status = Some("stale".to_string());
            best_candidate = Some(rating);
        }
    } else {
        println!("RYM-GET-RATING: ❌ Local cache miss");
    }
    
    // 2. Check Supabase
    println!("RYM-GET-RATING: Checking Supabase cache...");
    if let Some(supabase) = &state.supabase {
        if let Some(mut rating) = supabase.get_cached_rating(&artist, &album).await {
             let release_ts = parse_release_date_to_timestamp(&rating.release_date);
             let ttl = compute_ttl_seconds(now, release_ts);
             
             if is_fresh(rating.timestamp, ttl, now) {
                 println!("RYM-GET-RATING: ✓ SUPABASE CACHE HIT (FRESH)");
                 
                 // Save to local
                 println!("RYM-GET-RATING: Saving to local cache...");
                 let _ = state.db.lock().unwrap().save_rating(&rating);
                 
                 // Broadcast
                 let _ = app.emit("rym-rating-updated", rating.clone());
                 
                 rating.status = Some("fresh".to_string());
                 return Ok(Some(rating));
             } else {
                 println!("RYM-GET-RATING: ⚠️ SUPABASE CACHE HIT (STALE)");
                 
                 let use_supabase = match &best_candidate {
                     Some(local) => rating.timestamp > local.timestamp,
                     None => true,
                 };
                 
                 if use_supabase {
                     rating.status = Some("stale".to_string());
                     best_candidate = Some(rating);
                 }
             }
        }
    } else {
        println!("RYM-GET-RATING: Supabase client not configured");
    }
    
    // 3. Handle Miss / Stale
    
    // Check if user is on RYM tab
    let is_rym_visible = if let Some(w) = app.get_webview_window("rym") {
        w.is_visible().unwrap_or(false)
    } else {
        false
    };
    
    if is_rym_visible {
        println!("RYM-GET-RATING: User is on RYM tab. Initiating navigation/scrape...");
        
        let query = format!("{} {}", artist, album);
        let encoded_query = urlencoding::encode(&query);
        let search_url = format!("https://rateyourmusic.com/search?searchterm={}&searchtype=l", encoded_query);
        
        let app_handle = app.clone();
        // I'll just use the app_handle to get state inside the spawn
        tauri::async_runtime::spawn(async move {
            if navigate_to_rym_with_rate_limit(&app_handle, search_url).await.is_ok() {
                let state = app_handle.state::<AppState>();
                let mut init = state.rym_initialized.lock().unwrap();
                *init = true;
            }
        });
        
        if let Some(rating) = best_candidate {
             println!("RYM-GET-RATING: Returning stale candidate while refreshing...");
             return Ok(Some(rating));
        }
        
        return Ok(None);
    } else {
        println!("RYM-GET-RATING: User is NOT on RYM tab. Skipping automatic navigation.");
        if let Some(mut rating) = best_candidate {
            rating.status = Some("stale".to_string());
            println!("RYM-GET-RATING: Returning stale candidate (No Auto-Refresh).");
            return Ok(Some(rating));
        } else {
            return Ok(None);
        }
    }
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
    println!("RYM-SAVE-RATING:   - Rank: {:?}", rating.rank);
    println!("RYM-SAVE-RATING:   - Release Date: {}", rating.release_date);
    
    let track_count = rating.track_ratings.as_ref().map(|s| s.matches("title").count()).unwrap_or(0);
    let review_count = rating.reviews.as_ref().map(|s| s.matches("reviewer").count()).unwrap_or(0);
    println!("RYM-SAVE-RATING:   - Tracks Found: {}", track_count);
    println!("RYM-SAVE-RATING:   - Reviews Found: {}", review_count);
    println!("RYM-SAVE-RATING:   - Timestamp: {}", rating.timestamp);
    
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
            let initialized = state.rym_initialized.lock().unwrap();
            !*initialized
        }; 
        
        if needs_init {
            println!("RYM-INIT: First time showing RYM window (or previous init failed), loading homepage...");
            println!("RYM-SHOW: Calling navigate_to_rym_with_rate_limit...");
            
            match navigate_to_rym_with_rate_limit(&app, "https://rateyourmusic.com".to_string()).await {
                Ok(_) => {
                    println!("RYM-SHOW: ✓ Navigation completed successfully");
                    let mut initialized = state.rym_initialized.lock().unwrap();
                    *initialized = true;
                    let mut rym_info = state.rym_current_info.lock().unwrap();
                    *rym_info = Some("Home".to_string());
                },
                Err(e) => {
                    println!("RYM-SHOW: ❌ Navigation failed: {}", e);
                    // Do NOT set initialized to true, so it tries again next time
                }
            }
        } else {
             println!("RYM-SHOW: RYM already initialized, skipping navigation");
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
                
                // CRITICAL FIX: Also update the current music URL so subsequent 
                // URL-based checks (like normalization) correctly identify we are here.
                let mut current_url = state.current_music_url.lock().unwrap();
                *current_url = Some(url.clone());
                
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
             
             // Normalize check: handle geo.music vs music.apple
            let normalized_current = current_url.replace("geo.music.apple.com", "music.apple.com");
            let normalized_incoming = url.replace("geo.music.apple.com", "music.apple.com");
            
            if normalized_current == normalized_incoming {
                 println!("RYM-APPLE-MUSIC: ❌ Ignoring sync request - URLs match after normalization");
                 return;
            }
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
async fn sync_to_rym(artist: String, album: String, background: bool, force: bool, music_url: Option<String>, state: State<'_, AppState>, app: tauri::AppHandle) -> Result<(), String> {
    use release_date::{parse_release_date_to_timestamp, compute_ttl_seconds, is_fresh};

    let album_key = format!("{} - {}", artist, album);
    
    println!("RYM-SYNC: ========================================");
    println!("RYM-SYNC: Starting sync to RYM for: {}", album_key);
    println!("RYM-SYNC: Background mode: {}, Force: {}", background, force);
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
    let mut match_found = false;
    {
        let rym_info = state.rym_current_info.lock().unwrap();
        if let Some(current) = &*rym_info {
            // Relaxed check: Exact match OR Contains OR Case-insensitive
            match_found = current == &album_key 
                || current.contains(&album_key) 
                || album_key.contains(current)
                || current.to_lowercase() == album_key.to_lowercase();
                
            if match_found && !force && background {
                println!("RYM-SYNC: ❌ Skipping sync - RYM window already has {} (Match: {})", album_key, current);
                println!("RYM-SYNC: ========================================");
                return Ok(())
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
    }

    if let Some(rym_window) = app.get_webview_window("rym") {
        let now = chrono::Utc::now().timestamp();
        let mut best_candidate: Option<AlbumRating> = None;

        // STEP 1: Check local SQLite database first (Skip if force)
        let mut _cached_rating = None;
        if !force {
            println!("RYM-SYNC: Step 1 - Checking local SQLite database...");
            _cached_rating = {
                let db = state.db.lock().unwrap();
                db.get_rating(&album, &artist).ok().flatten()
            };

            if let Some(mut rating) = _cached_rating {
                let release_ts = parse_release_date_to_timestamp(&rating.release_date);
                let ttl = compute_ttl_seconds(now, release_ts);
                
                let tracks_json = rating.track_ratings.as_ref().map(|s| s.as_str()).unwrap_or("[]");
                let has_tracks = tracks_json.len() > 5;
                
                println!("RYM-SYNC: Found local cache entry.");
                println!("RYM-SYNC:   - Last fetched: {} ({} seconds ago)", rating.timestamp, now - rating.timestamp);
                println!("RYM-SYNC:   - TTL: {} seconds", ttl);
                println!("RYM-SYNC:   - Has tracks: {}", has_tracks);

                if is_fresh(rating.timestamp, ttl, now) && has_tracks {
                    println!("RYM-SYNC: ✓ LOCAL CACHE HIT (FRESH & COMPLETE)");
                    rating.status = Some("fresh".to_string());
                    
                    println!("RYM-SYNC: Broadcasting fresh cached data...");
                    let _ = app.emit("rym-rating-updated", rating);
                    
                    // If foreground, we still need to show the window
                    if !background {
                        if match_found {
                             println!("RYM-SYNC: Match already loaded. Showing window.");
                             let _ = rym_window.show();
                             let _ = rym_window.set_focus();
                             if let Some(m) = app.get_webview_window("music") { let _ = m.hide(); }
                             return Ok(());
                        }
                    } else {
                        return Ok(());
                    }
                    // If foreground but not match_found, continue to navigation
                } else {
                     let reason = if !has_tracks { "INCOMPLETE DATA" } else { "STALE" };
                     println!("RYM-SYNC: ⚠️ LOCAL CACHE HIT ({})", reason);
                     rating.status = Some("stale".to_string());
                     best_candidate = Some(rating);
                }
            } else {
                println!("RYM-SYNC: ❌ Local cache miss");
            }
                
            // STEP 2: Check Supabase if not fresh local
            println!("RYM-SYNC: Step 2 - Checking Supabase...");
            if let Some(supabase) = &state.supabase {
                if let Some(mut rating) = supabase.get_cached_rating(&artist, &album).await {
                     let release_ts = parse_release_date_to_timestamp(&rating.release_date);
                     let ttl = compute_ttl_seconds(now, release_ts);
                     
                     if is_fresh(rating.timestamp, ttl, now) {
                         println!("RYM-SYNC: ✓ SUPABASE CACHE HIT (FRESH)");
                         rating.status = Some("fresh".to_string());
                         
                         // Save to local
                         println!("RYM-SYNC: Saving Supabase data to local cache...");
                         let _ = state.db.lock().unwrap().save_rating(&rating);
                         
                         println!("RYM-SYNC: Broadcasting fresh cached data...");
                         let _ = app.emit("rym-rating-updated", rating);
                         
                         if !background {
                            if match_found {
                                 let _ = rym_window.show();
                                 let _ = rym_window.set_focus();
                                 if let Some(m) = app.get_webview_window("music") { let _ = m.hide(); }
                                 return Ok(());
                            }
                         } else {
                            return Ok(());
                         }
                     } else {
                         println!("RYM-SYNC: ⚠️ SUPABASE CACHE HIT (STALE)");
                         let use_supabase = match &best_candidate {
                             Some(local) => rating.timestamp > local.timestamp,
                             None => true,
                         };
                         if use_supabase {
                             rating.status = Some("stale".to_string());
                             best_candidate = Some(rating);
                         }
                     }
                }
            }
        }

        // STEP 3: Fallback / Miss Handling
        
        // Always emit current best status so UI can show buttons (Refresh/Open)
        if let Some(rating) = &best_candidate {
             println!("RYM-SYNC: Broadcasting current candidate status: {:?}", rating.status);
             let _ = app.emit("rym-rating-updated", rating.clone());
        } else {
             println!("RYM-SYNC: Broadcasting MISSING status to UI...");
             let missing_rating = AlbumRating {
                 album_name: album.clone(),
                 artist_name: artist.clone(),
                 rym_rating: 0.0,
                 rating_count: 0,
                 rym_url: "NO_MATCH".to_string(), 
                 genres: "".to_string(),
                 secondary_genres: None,
                 descriptors: None,
                 language: None,
                 rank: None,
                 track_ratings: None,
                 reviews: None,
                 release_date: "".to_string(),
                 timestamp: now,
                 status: Some("missing".to_string()),
             };
             let _ = app.emit("rym-rating-updated", missing_rating);
        }

        // DECIDE TO NAVIGATE
        let is_rym_visible = rym_window.is_visible().unwrap_or(false);
        
        // Rule: If background (auto-sync) AND not on RYM tab AND NOT force -> NO NAVIGATION
        // EXCEPTION: If data is MISSING (no best_candidate), we allow one background auto-nav to fetch it.
        if background && !is_rym_visible && !force && best_candidate.is_some() {
             println!("RYM-SYNC: Auto background sync & User NOT on RYM tab. Data exists but stale. Skipping navigation.");
             return Ok(())
        }
        
        // If we have a match already and not forcing, just show window and return
        // BUT only if we aren't already on the RYM tab (if we are, we might want to refresh)
        if !force && match_found && !background && !is_rym_visible {
             println!("RYM-SYNC: Match already loaded. Showing window (No Nav).");
             let _ = rym_window.show();
             let _ = rym_window.set_focus();
             if let Some(m) = app.get_webview_window("music") { let _ = m.hide(); }
             return Ok(())
        }
        
        if skip_rym_navigation {
             println!("RYM-SYNC: 🛑 Skipping RYM navigation (Hard Sync Blocker)");
             return Ok(())
        }
        
        // DETERMINE TARGET URL
        let target_url = if let Some(rating) = best_candidate {
            rating.rym_url
        } else {
            let query = format!(r"\ site:rateyourmusic.com/release {} {}", artist, album);
            let encoded_query = urlencoding::encode(&query);
            format!("https://duckduckgo.com/?q={}", encoded_query)
        };
        
        println!("RYM-SYNC: Navigating to refresh/find data: {}", target_url);
        let _ = rym_window.eval("localStorage.setItem('tauri_ignore_next_sync', 'true')");
        
        let safe_key = album_key.replace("'", r"'\'");
        let js_key = format!("localStorage.setItem('tauri_last_synced_album', '{}')", safe_key);
        let _ = rym_window.eval(&js_key);
        let _ = rym_window.eval("localStorage.setItem('tauri_sync_occurred', 'true')");
        
        if target_url.contains("duckduckgo") {
            let _ = rym_window.eval("localStorage.setItem('tauri_ddg_sync_active', 'true')");
        }
        
        if let Some(src_url) = &music_url {
             let safe_url = src_url.replace("'", r"'\'");
             let js = format!("localStorage.setItem('tauri_last_pending_url', '{}')", safe_url);
             let _ = rym_window.eval(&js);
        }
        
        navigate_to_rym_with_rate_limit(&app, target_url).await?;
        println!("RYM-SYNC: ✓ Navigation initiated");

        {
            let mut init = state.rym_initialized.lock().unwrap();
            *init = true;
            let mut rym_info = state.rym_current_info.lock().unwrap();
            *rym_info = Some(album_key.clone());
        }

        // Show toast notifications
        if background {
            let msg = format!("if (window.showSyncToast) window.showSyncToast(`Synced: {}`)", album.replace("`", r"\`"));
            let _ = rym_window.eval(&msg);
            if let Some(m) = app.get_webview_window("music") {
                let _ = m.eval(&msg);
            }
        } else {
            let _ = rym_window.show();
            let _ = rym_window.set_focus();
            if let Some(m) = app.get_webview_window("music") { let _ = m.hide(); }
        }
    } else {
        println!("RYM-SYNC: ❌ ERROR - RYM window not found!");
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
    let file_name = format!("{}_sample.html", safe_type);

    let file_path = path.join(file_name);
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

            let _app_handle_clone = app_handle.clone();
            
            // Setup Native Menu with Shortcuts
            let toggle_shortcut = MenuItem::with_id(app, "toggle", "Switch Tabs", true, Some("CmdOrCtrl+Shift+["))?;
            let toggle_shortcut_alt = MenuItem::with_id(app, "toggle_alt", "Switch Tabs", true, Some("CmdOrCtrl+Shift+] "))?;
            let devtools_shortcut = MenuItem::with_id(app, "devtools", "Open DevTools", true, Some("CmdOrCtrl+Option+I"))?;
            let back_shortcut = MenuItem::with_id(app, "back", "Back", true, Some("CmdOrCtrl+["))?;
            let forward_shortcut = MenuItem::with_id(app, "forward", "Forward", true, Some("CmdOrCtrl+] "))?;
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
                    if let Some(w) = app.get_webview_window("player") { let _ = w.open_devtools(); }
                } else if event.id() == "quit" {
                    std::process::exit(0);
                }
            });

            // Universal Injection Script
            let universal_script = r#"
                (function() {
                    const WINDOW_LABEL = window.TAURI_WINDOW_LABEL;
                    console.log('RYM-APPLE-MUSIC: Universal Engine Starting for [' + WINDOW_LABEL + ']');
                    
                    const STYLE_ID = 'tauri-universal-style';
                    const CONTAINER_ID = 'tauri-tabs';
                    const TOAST_ID = 'tauri-toast';
                    const IS_MUSIC_HOST = window.location.host.includes('apple.com');
                    const IS_RYM = window.location.host.includes('rateyourmusic.com');
                    const IS_PLAYER = WINDOW_LABEL === 'player';
                    const IS_BROWSER = (WINDOW_LABEL === 'music' || WINDOW_LABEL === 'rym');

                    window.showSyncToast = function(msg) {
                        const toast = document.getElementById(TOAST_ID);
                        if (toast) {
                            toast.textContent = msg;
                            toast.classList.add('show');
                            setTimeout(function() { toast.classList.remove('show'); }, 4500);
                        }
                    };

                    function setupManualDrag() {
                        if (window.hasManualDrag) return;
                        window.hasManualDrag = true;
                        window.addEventListener('mousedown', function(e) {
                            const path = e.composedPath();
                            const isInteractive = path.some(function(el) {
                                if (!el.tagName) return false;
                                const tag = el.tagName.toLowerCase();
                                const role = el.getAttribute ? el.getAttribute('role') : null;
                                const cls = typeof el.className === 'string' ? el.className : "";
                                const id = typeof el.id === 'string' ? el.id : "";
                                return tag === 'a' || tag === 'button' || tag === 'input' || tag === 'select' || tag === 'textarea' ||
                                       tag === 'time' || tag === 'amp-lcd' ||
                                       role === 'button' || role === 'link' || role === 'slider' ||
                                       cls.includes('progress') || cls.includes('lcd') ||
                                       id.includes('progress') || id.includes('playback');
                            });
                            const isDragRegion = path.some(function(el) { return el.hasAttribute && el.hasAttribute('data-tauri-drag'); });
                            const edgeMargin = 8;
                            const isNearEdge = e.clientX < edgeMargin || e.clientX > window.innerWidth - edgeMargin ||
                                             e.clientY < edgeMargin || e.clientY > window.innerHeight - edgeMargin;

                            if (isDragRegion && !isInteractive && !isNearEdge) {
                                e.stopImmediatePropagation();
                                e.preventDefault();
                                const invoke = window.__TAURI__.core ? window.__TAURI__.core.invoke : window.__TAURI__.invoke;
                                if (invoke) invoke('start_drag').catch(function(err) { console.error(err); });
                            }
                        }, true);
                    }

                    function inject() {
                        if (!document.body) return;
                        setupManualDrag();

                        if (!document.getElementById(STYLE_ID)) {
                            const style = document.createElement('style');
                            style.id = STYLE_ID;
                            let css = '[data-tauri-drag] { -webkit-app-region: drag !important; cursor: default !important; z-index: 2147483645 !important; } ' +
                                      '[data-tauri-no-drag] { -webkit-app-region: no-drag !important; pointer-events: auto !important; z-index: 2147483646 !important; } ' +
                                      '#tauri-toast { position: fixed !important; bottom: 110px !important; left: 50% !important; transform: translateX(-50%) !important; background: #fb233b !important; color: white !important; padding: 12px 24px !important; border-radius: 30px !important; z-index: 2147483647 !important; visibility: hidden; opacity: 0; transition: all 0.3s ease; } ' +
                                      '#tauri-toast.show { visibility: visible; opacity: 1; } ';

                            if (IS_PLAYER) {
                                css += '#apple-music-player, .player-bar, amp-chrome-player { position: fixed !important; top: 0 !important; left: 0 !important; width: 100% !important; height: 54px !important; margin: 0 !important; z-index: 2147483647 !important; visibility: visible !important; opacity: 1 !important; background: #14141a !important; -webkit-app-region: drag !important; } ' +
                                       '#apple-music-player *, .player-bar *, amp-chrome-player * { -webkit-app-region: no-drag !important; } ' +
                                       'html, body { background: transparent !important; overflow: hidden !important; } ' +
                                       '.sidebar, .header, .web-navigation, .web-nav-logo, .logo, .nav-header { display: none !important; } ';
                            }

                            if (IS_BROWSER) {
                                if (IS_MUSIC_HOST) {
                                    css += '.logo, .web-nav-logo, .nav-header, .logo a { display: none !important; } ';
                                }
                                css += '#tauri-tabs { position: fixed !important; bottom: 20px !important; left: 50% !important; transform: translateX(-50%) !important; z-index: 2147483647 !important; display: flex !important; gap: 8px !important; background: rgba(20, 20, 20, 0.6) !important; padding: 6px 12px !important; border-radius: 20px !important; border: 1px solid rgba(251, 35, 59, 0.5) !important; backdrop-filter: blur(15px) !important; box-shadow: 0 4px 15px rgba(0,0,0,0.5) !important; -webkit-app-region: no-drag !important; pointer-events: auto !important; opacity: 0.8 !important; transition: all 0.3s ease !important; } ' +
                                       '#tauri-tabs:hover { opacity: 1; transform: translateX(-50%) translateY(-2px); background: rgba(30,30,30,0.9); } ' +
                                       '.tauri-tab-btn { all: unset !important; display: inline-block !important; box-sizing: border-box !important; background: transparent !important; color: white !important; padding: 4px 12px !important; border-radius: 12px !important; cursor: pointer !important; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif !important; font-size: 11px !important; font-weight: 700 !important; opacity: 0.5; transition: all 0.2s ease !important; } ' +
                                       '.tauri-tab-btn.active { background: #fb233b !important; opacity: 1; } ' +
                                       '#tauri-actions { position: fixed !important; bottom: 20px !important; right: 20px !important; z-index: 2147483647 !important; display: flex !important; gap: 8px !important; pointer-events: none !important; } ' +
                                       '#tauri-actions button { pointer-events: auto !important; background: rgba(20, 20, 20, 0.8) !important; backdrop-filter: blur(15px) !important; border: 1px solid rgba(251, 35, 59, 0.4) !important; color: white !important; padding: 8px 16px !important; border-radius: 20px !important; cursor: pointer !important; font-size: 12px !important; font-weight: 700 !important; box-shadow: 0 4px 15px rgba(0,0,0,0.4) !important; transition: all 0.2s ease !important; } ' +
                                       '#tauri-actions button:hover { transform: translateY(-2px); background: rgba(30, 30, 30, 0.9); border-color: #fb233b !important; } ';
                                
                                if (IS_MUSIC_HOST) {
                                    css += '.player-bar, amp-chrome-player { display: none !important; } ';
                                }

                                if (IS_RYM) {
                                    css += 'header#page-header, header#page_header, .header, #page_header { display: flex !important; height: 54px !important; -webkit-app-region: drag !important; z-index: 2147483640 !important; } ' +
                                           '.header_inner { display: flex !important; align-items: center !important; justify-content: space-between !important; margin-left: 50px !important; width: calc(100% - 50px) !important; -webkit-app-region: drag !important; } ' +
                                           '.header_inner > div, .header_inner > a, .header_inner > button, .header_inner input { -webkit-app-region: no-drag !important; pointer-events: auto !important; } ';
                                }
                            }
                            style.textContent = css;
                            (document.head || document.documentElement).appendChild(style);
                        }

                        // VIGOROUSLY UPDATE DRAG REGIONS (Restore original robust logic)
                        const bars = Array.from(document.querySelectorAll('header, nav, [class*="player-bar"], [class*="header"], [id*="header"], amp-chrome-player, .logo, #page_header, .chrome-player'));
                        bars.forEach(el => {
                            if (el.id === CONTAINER_ID) return;
                            const style = window.getComputedStyle(el);
                            const isFixed = style.position === 'fixed' || style.position === 'sticky';
                            const rect = el.getBoundingClientRect();
                            const isAtEdge = isFixed && (rect.top <= 10 || (window.innerHeight - rect.bottom) <= 10);
                            const isHeaderType = /header|player|logo|nav/i.test(el.className + el.id + el.tagName);

                            if (isAtEdge || isHeaderType) {
                                if (!el.hasAttribute('data-tauri-drag')) el.setAttribute('data-tauri-drag', '');
                                if (el.style.cursor !== 'default') el.style.cursor = 'default';
                            }
                        });

                        document.querySelectorAll('[data-tauri-drag] a, [data-tauri-drag] button, [data-tauri-drag] input, [data-tauri-drag] [role="button"], [data-tauri-drag] amp-lcd, [data-tauri-drag] amp-chrome-volume, [data-tauri-drag] amp-lcd-progress, [data-tauri-drag] .ui_search, [data-tauri-drag] .header_item').forEach(el => {
                            if (!el.hasAttribute('data-tauri-no-drag')) el.setAttribute('data-tauri-no-drag', '');
                        });

                        if (IS_PLAYER) {
                            const bar = document.querySelector('.player-bar') || document.querySelector('amp-chrome-player');
                            if (bar && !bar.hasAttribute('data-tauri-drag')) bar.setAttribute('data-tauri-drag', '');
                        }

                        if (IS_BROWSER) {
                            if (IS_MUSIC_HOST) {
                                const logo = document.querySelector('.logo') || document.querySelector('.web-nav-logo');
                                if (logo) logo.style.display = 'none';
                                const logoA = document.querySelector('.logo a');
                                if (logoA) logoA.remove();
                                const navHeader = document.querySelector('.nav-header');
                                if (navHeader) navHeader.style.display = 'none';
                            }
                            const bodyContainer = document.querySelector('.body-container');
                            if (bodyContainer && !bodyContainer.hasAttribute('data-tauri-drag')) bodyContainer.setAttribute('data-tauri-drag', '');

                            if (!document.getElementById(CONTAINER_ID)) {
                                if (!document.getElementById(TOAST_ID)) {
                                    const toast = document.createElement('div');
                                    toast.id = TOAST_ID;
                                    document.body.appendChild(toast);
                                }
                                const container = document.createElement('div');
                                container.id = CONTAINER_ID;
                                const isMusic = WINDOW_LABEL === 'music';
                                const musicBtn = document.createElement('button');
                                musicBtn.className = 'tauri-tab-btn' + (isMusic ? ' active' : '');
                                musicBtn.textContent = 'Music';
                                musicBtn.onclick = function() { window.__TAURI__.core.invoke('show_music'); };
                                const rymBtn = document.createElement('button');
                                rymBtn.className = 'tauri-tab-btn' + (!isMusic ? ' active' : '');
                                rymBtn.textContent = 'RYM';
                                rymBtn.onclick = function() { window.__TAURI__.core.invoke('show_rym'); };
                                container.appendChild(musicBtn);
                                container.appendChild(rymBtn);
                                document.body.appendChild(container);
                            }
                        }
                    }

                    if (IS_BROWSER) {
                        window.extractMusicInfo = function() {
                            let album = document.querySelector('.headings__title span[dir="auto"]')?.innerText || document.querySelector('[data-testid="non-editable-product-title"] span')?.innerText;
                            let artists = Array.from(document.querySelectorAll('.headings__subtitles a, [data-testid="product-subtitles"] a')).map(function(a) { return a.innerText.trim(); });
                            let artist = artists.join(' ');
                            if (artist && album) return { artist: artist.trim(), album: album.trim() };
                            return null;
                        };

                        setInterval(function() {
                            if (IS_MUSIC_HOST) {
                                const info = window.extractMusicInfo();
                                if (info) {
                                    const albumKey = info.artist + ' - ' + info.album;
                                    if (albumKey !== localStorage.getItem('tauri_last_synced_album')) {
                                        localStorage.setItem('tauri_last_synced_album', albumKey);
                                        window.__TAURI__.core.invoke('sync_to_rym', { artist: info.artist, album: info.album, background: true, force: false, musicUrl: window.location.href });
                                    }
                                }
                            }
                        }, 3000);
                    }

                    inject();
                    const observer = new MutationObserver(function() { inject(); });
                    observer.observe(document.body || document.documentElement, { childList: true, subtree: true });
                })();
            "#;

            // window creation logic
            let music_init = ["window.TAURI_WINDOW_LABEL = 'music'; ", universal_script].concat();
            let rym_init = ["window.TAURI_WINDOW_LABEL = 'rym'; ", universal_script].concat();
            let player_init = ["window.TAURI_WINDOW_LABEL = 'player'; ", universal_script].concat();

            let music_window = tauri::WebviewWindowBuilder::new(app, "music", tauri::WebviewUrl::External("https://music.apple.com".parse().unwrap()))
                .title("RYM Apple Music Player")
                .data_store_identifier(*b"music_store_v1_0")
                .inner_size(1200.0, 600.0)
                .center()
                .visible(true)
                .hidden_title(true)
                .resizable(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .background_color(Color(20, 20, 26, 255))
                .devtools(true)
                .initialization_script(&music_init)
                .build()
                .expect("Failed to create music window");

            let rym_window = tauri::WebviewWindowBuilder::new(app, "rym", tauri::WebviewUrl::External("about:blank".parse().unwrap()))
                .title("RYM Apple Music Player")
                .data_store_identifier(*b"rym_store_v1_000")
                .inner_size(1200.0, 600.0)
                .center()
                .visible(false)
                .hidden_title(true)
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .background_color(Color(20, 20, 26, 255))
                .devtools(true)
                .initialization_script(&rym_init)
                .build()
                .expect("Failed to create RYM window");
            
            let player_window = tauri::WebviewWindowBuilder::new(app, "player", tauri::WebviewUrl::External("https://music.apple.com".parse().unwrap()))
                .title("Apple Music Player")
                .data_store_identifier(*b"music_store_v1_0")
                .inner_size(960.0, 54.0)
                .visible(true)
                .decorations(false)
                .transparent(true)
                .resizable(false)
                .devtools(true)
                .initialization_script(&player_init)
                .build()
                .expect("Failed to create player window");
            
            music_window.open_devtools();
            rym_window.open_devtools();
            player_window.open_devtools();

            let rym_window_clone = rym_window.clone();
            let player_window_for_music = player_window.clone();
            let music_window_for_event = music_window.clone();
            music_window.on_window_event(move |event| {
                if !music_window_for_event.is_visible().unwrap_or(false) { return; }
                match event {
                    tauri::WindowEvent::Resized(size) => { 
                        let _ = rym_window_clone.set_size(*size);
                        let _ = player_window_for_music.set_size(tauri::Size::Logical(tauri::LogicalSize { width: size.width as f64, height: 54.0 }));
                    }
                    tauri::WindowEvent::Moved(pos) => { 
                        let _ = rym_window_clone.set_position(*pos); 
                        if let Ok(size) = music_window_for_event.inner_size() {
                            let _ = player_window_for_music.set_position(tauri::Position::Physical(tauri::PhysicalPosition { 
                                x: pos.x, 
                                y: pos.y + size.height as i32 
                            }));
                        }
                    }
                    _ => {}
                }
            });

            let music_window_clone2 = music_window.clone();
            let player_window_for_rym = player_window.clone();
            let rym_window_for_event = rym_window.clone();
            rym_window.on_window_event(move |event| {
                if !rym_window_for_event.is_visible().unwrap_or(false) { return; }
                match event {
                    tauri::WindowEvent::Resized(size) => { 
                        let _ = music_window_clone2.set_size(*size); 
                        let _ = player_window_for_rym.set_size(tauri::Size::Logical(tauri::LogicalSize { width: size.width as f64, height: 54.0 }));
                    }
                    tauri::WindowEvent::Moved(pos) => { 
                        let _ = music_window_clone2.set_position(*pos); 
                        if let Ok(size) = rym_window_for_event.inner_size() {
                            let _ = player_window_for_rym.set_position(tauri::Position::Physical(tauri::PhysicalPosition { 
                                x: pos.x, 
                                y: pos.y + size.height as i32 
                            }));
                        }
                    }
                    _ => {}
                }
            });
            
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_rym_rating, save_rym_rating, show_music, show_rym, set_pending_music_url, sync_to_rym, go_back, go_forward, save_sample_html, start_drag, set_manual_match])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}