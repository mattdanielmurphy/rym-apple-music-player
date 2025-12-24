use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RymAlbumData {
    pub rating: f32,
    pub rating_count: i32,
    pub url: String,
}

pub async fn search_rym_album(artist: &str, album: &str) -> Result<Option<RymAlbumData>, String> {
    // Construct RYM search URL
    let search_query = format!("{} {}", artist, album);
    let encoded_query = urlencoding::encode(&search_query);
    let search_url = format!("https://rateyourmusic.com/search?searchterm={}&searchtype=l", encoded_query);
    
    // Fetch the search page
    let client = reqwest::Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;
    
    let response = client
        .get(&search_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch RYM search page: {}", e))?;
    
    let html = response
        .text()
        .await
        .map_err(|e| format!("Failed to read response text: {}", e))?;
    
    // Extract album path from search results
    let album_path = {
        let document = Html::parse_document(&html);
        let album_link_selector = Selector::parse("a.searchpage").unwrap();
        
        if let Some(first_result) = document.select(&album_link_selector).next() {
            first_result.value().attr("href")
                .ok_or("No href found in album link")?
                .to_string()
        } else {
            return Ok(None);
        }
    }; // document is dropped here
    
    let album_url = format!("https://rateyourmusic.com{}", album_path);
    
    // Fetch the album page
    let album_response = client
        .get(&album_url)
        .send()
        .await
        .map_err(|e| format!("Failed to fetch album page: {}", e))?;
    
    let album_html = album_response
        .text()
        .await
        .map_err(|e| format!("Failed to read album page: {}", e))?;
    
    // Extract rating data from album page
    let (rating, rating_count) = {
        let album_doc = Html::parse_document(&album_html);
        
        // Extract rating
        let rating_selector = Selector::parse(".avg_rating").unwrap();
        let rating_text = album_doc
            .select(&rating_selector)
            .next()
            .and_then(|el| el.text().next())
            .ok_or("Rating not found")?
            .to_string();
        
        let rating: f32 = rating_text
            .trim()
            .parse()
            .map_err(|_| format!("Failed to parse rating: {}", rating_text))?;
        
        // Extract rating count
        let count_selector = Selector::parse(".num_ratings").unwrap();
        let count_text = album_doc
            .select(&count_selector)
            .next()
            .and_then(|el| el.text().next())
            .ok_or("Rating count not found")?
            .to_string();
        
        // Remove commas and parse
        let count_clean = count_text.replace(",", "").trim().to_string();
        let rating_count: i32 = count_clean
            .parse()
            .map_err(|_| format!("Failed to parse rating count: {}", count_text))?;
        
        (rating, rating_count)
    }; // album_doc is dropped here
    
    Ok(Some(RymAlbumData {
        rating,
        rating_count,
        url: album_url,
    }))
}
