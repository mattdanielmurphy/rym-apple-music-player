use rusqlite::{Connection, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlbumRating {
    pub album_name: String,
    pub artist_name: String,
    pub rym_rating: f32,
    pub rating_count: i32,
    pub rym_url: String,
    pub genres: String,
    pub secondary_genres: Option<String>,
    pub descriptors: Option<String>,
    pub language: Option<String>,
    pub rank: Option<String>,
    pub track_ratings: Option<String>, // JSON
    pub reviews: Option<String>,       // JSON
    pub release_date: String,
    pub timestamp: i64,
    #[serde(skip_deserializing, default)]
    pub status: Option<String>,
}

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn new(db_path: PathBuf) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        
        // Create table if it doesn't exist
        conn.execute(
            "CREATE TABLE IF NOT EXISTS album_ratings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                album_name TEXT NOT NULL,
                artist_name TEXT NOT NULL,
                rym_rating REAL NOT NULL,
                rating_count INTEGER NOT NULL,
                rym_url TEXT NOT NULL,
                genres TEXT NOT NULL DEFAULT '',
                secondary_genres TEXT DEFAULT '',
                descriptors TEXT DEFAULT '',
                language TEXT DEFAULT '',
                rank TEXT DEFAULT '',
                track_ratings TEXT DEFAULT '',
                reviews TEXT DEFAULT '',
                release_date TEXT NOT NULL DEFAULT '',
                timestamp INTEGER NOT NULL,
                UNIQUE(album_name, artist_name)
            )",
            [],
        )?;

        // COMPREHENSIVE MIGRATIONS
        let columns = [
            ("genres", "TEXT NOT NULL DEFAULT ''"),
            ("secondary_genres", "TEXT DEFAULT ''"),
            ("descriptors", "TEXT DEFAULT ''"),
            ("language", "TEXT DEFAULT ''"),
            ("rank", "TEXT DEFAULT ''"),
            ("track_ratings", "TEXT DEFAULT ''"),
            ("reviews", "TEXT DEFAULT ''"),
            ("release_date", "TEXT NOT NULL DEFAULT ''"),
        ];

        for (col, def) in columns {
            let query = format!("ALTER TABLE album_ratings ADD COLUMN {} {}", col, def);
            let _ = conn.execute(&query, []);
        }
        
        Ok(Database { conn })
    }
    
    pub fn get_rating(&self, album_name: &str, artist_name: &str) -> Result<Option<AlbumRating>> {
        // Robust normalization helper
        let normalize = |s: &str| {
            let s = s.to_lowercase();
            // Remove everything in brackets or parentheses
            let mut result = String::new();
            let mut depth = 0;
            for c in s.chars() {
                match c {
                    '(' | '[' => depth += 1,
                    ')' | ']' => depth = (depth as i32 - 1).max(0) as u32,
                    _ if depth == 0 => result.push(c),
                    _ => {}
                }
            }
            // Keep only alphanumeric
            result.chars().filter(|c| c.is_alphanumeric()).collect::<String>()
        };

        let norm_album = normalize(album_name);
        let norm_artist = normalize(artist_name);

        println!("RYM-DATABASE: Querying \"{}\" by \"{}\"", album_name, artist_name);
        println!("RYM-DATABASE: Normalized search key: \"{}\" | \"{}\"", norm_album, norm_artist);

        // 1. Try exact match (LOWER to handle case-insensitivity)
        let mut stmt = self.conn.prepare(
            "SELECT album_name, artist_name, rym_rating, rating_count, rym_url, genres, 
                    secondary_genres, descriptors, language, rank, track_ratings, reviews,
                    release_date, timestamp 
             FROM album_ratings 
             WHERE LOWER(album_name) = LOWER(?1) AND LOWER(artist_name) = LOWER(?2)"
        )?;
        
        let row = stmt.query_row([album_name, artist_name], |row| self.map_row(row));
        
        if let Ok(r) = row {
            println!("RYM-DATABASE: ✓ Found exact match (case-insensitive).");
            return Ok(Some(r));
        }

        // 2. Fuzzy match
        let mut stmt = self.conn.prepare("SELECT * FROM album_ratings")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let db_album: String = row.get(1)?;
            let db_artist: String = row.get(2)?;
            
            if normalize(&db_album) == norm_album && normalize(&db_artist) == norm_artist {
                println!("RYM-DATABASE: ✓ Found match via fuzzy normalization: \"{}\"", db_album);
                return Ok(Some(self.map_row(row)?));
            }
        }

        println!("RYM-DATABASE: ❌ No match found.");
        Ok(None)
    }

    fn map_row(&self, row: &rusqlite::Row) -> rusqlite::Result<AlbumRating> {
        Ok(AlbumRating {
            album_name: row.get(0)?,
            artist_name: row.get(1)?,
            rym_rating: row.get(2)?,
            rating_count: row.get(3)?,
            rym_url: row.get(4)?,
            genres: row.get(5)?,
            secondary_genres: row.get(6)?,
            descriptors: row.get(7)?,
            language: row.get(8)?,
            rank: row.get(9)?,
            track_ratings: row.get(10)?,
            reviews: row.get(11)?,
            release_date: row.get(12)?,
            timestamp: row.get(13)?,
            status: None,
        })
    }
    
    pub fn save_rating(&self, rating: &AlbumRating) -> Result<()> {
        println!("RYM-DATABASE: Saving/Updating \"{}\" by \"{}\"", rating.album_name, rating.artist_name);
        
        // We use INSERT OR REPLACE, but because the keys might have changed slightly due to cleaning,
        // we should actually check if a similar entry exists and update it, or just rely on the UNIQUE constraint
        // if we are consistent with our cleaning.
        
        match self.conn.execute(
            "INSERT OR REPLACE INTO album_ratings 
             (album_name, artist_name, rym_rating, rating_count, rym_url, genres, 
              secondary_genres, descriptors, language, rank, track_ratings, reviews,
              release_date, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            (
                &rating.album_name,
                &rating.artist_name,
                rating.rym_rating as f64,
                rating.rating_count as i64,
                &rating.rym_url,
                &rating.genres,
                &rating.secondary_genres,
                &rating.descriptors,
                &rating.language,
                &rating.rank,
                &rating.track_ratings,
                &rating.reviews,
                &rating.release_date,
                rating.timestamp,
            ),
        ) {
            Ok(_) => {
                println!("RYM-DATABASE: Successfully saved to cache.");
                Ok(())
            },
            Err(e) => {
                eprintln!("RYM-DATABASE: Failed to save to local cache: {}", e);
                Err(e)
            }
        }
    }
}
