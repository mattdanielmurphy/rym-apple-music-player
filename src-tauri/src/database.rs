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
    pub release_date: String,
    pub timestamp: i64,
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
                release_date TEXT NOT NULL DEFAULT '',
                timestamp INTEGER NOT NULL,
                UNIQUE(album_name, artist_name)
            )",
            [],
        )?;

        // Simple migrations for existing databases
        let _ = conn.execute("ALTER TABLE album_ratings ADD COLUMN genres TEXT NOT NULL DEFAULT ''", []);
        let _ = conn.execute("ALTER TABLE album_ratings ADD COLUMN release_date TEXT NOT NULL DEFAULT ''", []);
        
        Ok(Database { conn })
    }
    
    pub fn get_rating(&self, album_name: &str, artist_name: &str) -> Result<Option<AlbumRating>> {
        let mut stmt = self.conn.prepare(
            "SELECT album_name, artist_name, rym_rating, rating_count, rym_url, genres, release_date, timestamp 
             FROM album_ratings 
             WHERE album_name = ?1 AND artist_name = ?2"
        )?;
        
        let rating = stmt.query_row([album_name, artist_name], |row| {
            Ok(AlbumRating {
                album_name: row.get(0)?,
                artist_name: row.get(1)?,
                rym_rating: row.get(2)?,
                rating_count: row.get(3)?,
                rym_url: row.get(4)?,
                genres: row.get(5)?,
                release_date: row.get(6)?,
                timestamp: row.get(7)?,
            })
        });
        
        match rating {
            Ok(r) => Ok(Some(r)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
    
    pub fn save_rating(&self, rating: &AlbumRating) -> Result<()> {
        println!("RYM-DATABASE: Saving rating for {} - {}", rating.artist_name, rating.album_name);
        match self.conn.execute(
            "INSERT OR REPLACE INTO album_ratings 
             (album_name, artist_name, rym_rating, rating_count, rym_url, genres, release_date, timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            (
                &rating.album_name,
                &rating.artist_name,
                rating.rym_rating as f64,
                rating.rating_count as i64,
                &rating.rym_url,
                &rating.genres,
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
