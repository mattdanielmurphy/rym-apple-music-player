use serde::{Deserialize, Serialize};
use crate::database::AlbumRating;
use reqwest::Client;
use std::env;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SupabaseRating {
    artist_name: String,
    album_name: String,
    rym_rating: f32,
    rating_count: i32,
    rym_url: String,
    genres: String,
    secondary_genres: Option<String>,
    descriptors: Option<String>,
    language: Option<String>,
    rank: Option<String>,
    track_ratings: Option<String>,
    reviews: Option<String>,
    release_date: String,
}

#[derive(Clone)]
pub struct SupabaseClient {
    url: String,
    key: String,
    client: Client,
}

impl SupabaseClient {
    pub fn from_env() -> Option<Self> {
        let url = match env::var("SUPABASE_URL") {
            Ok(val) => {
                println!("RYM-SUPABASE: Found SUPABASE_URL");
                val
            },
            Err(_) => {
                println!("RYM-SUPABASE: ❌ Missing SUPABASE_URL environment variable");
                return None;
            }
        };

        let key = match env::var("SUPABASE_ANON_KEY") {
            Ok(val) => {
                println!("RYM-SUPABASE: Found SUPABASE_ANON_KEY");
                val
            },
            Err(_) => {
                println!("RYM-SUPABASE: ❌ Missing SUPABASE_ANON_KEY environment variable");
                return None;
            }
        };

        Some(Self {
            url,
            key,
            client: Client::new(),
        })
    }

    pub async fn get_cached_rating(&self, artist: &str, album: &str) -> Option<AlbumRating> {
        let table = "RYM-APPLE-MUSIC-PLAYER_ratings";
        let url = format!("{}/rest/v1/{}?artist_name=eq.{}&album_name=eq.{}", 
            self.url, table, urlencoding::encode(artist), urlencoding::encode(album));

        let res = self.client.get(&url)
            .header("apikey", &self.key)
            .header("Authorization", format!("Bearer {}", self.key))
            .send()
            .await
            .ok()?;

        let ratings: Vec<SupabaseRating> = res.json().await.ok()?;
        
        ratings.into_iter().next().map(|r| AlbumRating {
            artist_name: r.artist_name,
            album_name: r.album_name,
            rym_rating: r.rym_rating,
            rating_count: r.rating_count,
            rym_url: r.rym_url,
            genres: r.genres,
            secondary_genres: r.secondary_genres,
            descriptors: r.descriptors,
            language: r.language,
            rank: r.rank,
            track_ratings: r.track_ratings,
            reviews: r.reviews,
            release_date: r.release_date,
            timestamp: chrono::Utc::now().timestamp(),
            status: None,
        })
    }

    pub async fn save_rating(&self, rating: &AlbumRating) -> Result<(), String> {
        let table = "RYM-APPLE-MUSIC-PLAYER_ratings";
        let url = format!("{}/rest/v1/{}", self.url, table);

        let data = SupabaseRating {
            artist_name: rating.artist_name.clone(),
            album_name: rating.album_name.clone(),
            rym_rating: rating.rym_rating,
            rating_count: rating.rating_count,
            rym_url: rating.rym_url.clone(),
            genres: rating.genres.clone(),
            secondary_genres: rating.secondary_genres.clone(),
            descriptors: rating.descriptors.clone(),
            language: rating.language.clone(),
            rank: rating.rank.clone(),
            track_ratings: rating.track_ratings.clone(),
            reviews: rating.reviews.clone(),
            release_date: rating.release_date.clone(),
        };

        // UPSERT using ON CONFLICT (artist_name, album_name)
        let _ = self.client.post(&url)
            .header("apikey", &self.key)
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .header("Prefer", "resolution=merge-duplicates")
            .json(&data)
            .send()
            .await
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}
