import { invoke } from "@tauri-apps/api/core"
import { listen } from "@tauri-apps/api/event"

// UI Elements
const artistEl = document.getElementById("current-artist")
const albumEl = document.getElementById("current-album")
const scoreEl = document.getElementById("rym-score")
const countEl = document.getElementById("rym-count")
const linkEl = document.getElementById("rym-link") as HTMLAnchorElement
const containerEl = document.getElementById("rating-container")
const loadingEl = document.getElementById("loading-status")

let currentArtist = ""
let currentAlbum = ""

async function updateRYMData(artist: string, album: string) {
	if (artist === currentArtist && album === currentAlbum) return

	currentArtist = artist
	currentAlbum = album

	if (artistEl) artistEl.textContent = artist
	if (albumEl) albumEl.textContent = album

	if (containerEl) containerEl.style.display = "none"
	if (loadingEl) loadingEl.style.display = "block"

	try {
		console.log(`Fetching rating for ${artist} - ${album}`)
		const rating = await invoke("get_rym_rating", { artist, album })

		if (rating) {
			displayRating(rating as any)
		} else {
			console.log("Rating not in cache, waiting for scraper...")
		}
	} catch (e) {
		console.error("Failed to get rating:", e)
		if (loadingEl) loadingEl.textContent = "Error fetching rating"
	}
}

function displayRating(data: { rym_rating: number; rating_count: number; rym_url: string }) {
	if (loadingEl) loadingEl.style.display = "none"
	if (containerEl) containerEl.style.display = "block"

	if (scoreEl) scoreEl.textContent = data.rym_rating.toFixed(2)
	if (countEl) countEl.textContent = `${data.rating_count.toLocaleString()} ratings`
	if (linkEl) {
		linkEl.href = data.rym_url
		linkEl.textContent = "View on RYM"
	}
}

// Make it available globally for testing
;(window as any).updateRYMData = updateRYMData

async function setupApp() {
	// Listen for data from the hidden scraper
	await listen("rym-data-scraped", async (event: any) => {
		const data = event.payload as { rating: number; rating_count: number; url: string }
		console.log("Received data from scraper:", data)

		const ratingObj = {
			album_name: currentAlbum,
			artist_name: currentArtist,
			rym_rating: data.rating,
			rating_count: data.rating_count,
			rym_url: data.url,
			timestamp: Math.floor(Date.now() / 1000),
		}

		// Save to cache via Rust
		await invoke("save_rym_rating", { rating: ratingObj })

		// Update UI
		displayRating(ratingObj)
	})

	// Initialize MusicKit JS
	// Note: This usually requires a developer token.
	// The user mentioned "no dev account needed", which might refer to using
	// a public embed or a specific guest mode if implemented.
	try {
		// @ts-ignore
		if (window.MusicKit) {
			// @ts-ignore
			await MusicKit.configure({
				developerToken: "GUEST", // Placeholder or as requested
				app: {
					name: "RYM Apple Music Player",
					build: "1.0.0",
				},
			})
			console.log("MusicKit JS configured")
		}
	} catch (e) {
		console.warn("MusicKit JS configuration failed (expected without token):", e)
	}

	console.log("App setup complete. Waiting for album detection...")

	// Hook up manual search
	const btnSearch = document.getElementById("btn-search")
	const inputArtist = document.getElementById("manual-artist") as HTMLInputElement
	const inputAlbum = document.getElementById("manual-album") as HTMLInputElement

	if (btnSearch && inputArtist && inputAlbum) {
		btnSearch.addEventListener("click", () => {
			const artist = inputArtist.value.trim()
			const album = inputAlbum.value.trim()
			if (artist && album) {
				updateRYMData(artist, album)
			}
		})
	}

	// Periodically poll for common album info if we were in the same origin
	// Since we aren't, we'll rely on the user manually clicking or future improvements.
	// For local dev, you can call window.updateRYMData('Artist', 'Album')
}

window.addEventListener("DOMContentLoaded", () => {
	setupApp()
})
