use anyhow::Result;
use serde::Deserialize;

/// Perform a Google search using SerpAPI
pub async fn google_search(query: &str, num_results: usize) -> Result<String> {
    // Get API key from environment
    let api_key = match std::env::var("SERPAPI_KEY") {
        Ok(key) => key,
        Err(_) => {
            return Ok(format!(
                "Google search unavailable: SERPAPI_KEY not set in environment.\n\
                Query was: '{}'",
                query
            ));
        }
    };

    let num_results = num_results.min(20).max(1);

    // Build the search URL
    let url = format!(
        "https://serpapi.com/search.json?q={}&num={}&api_key={}",
        urlencoding::encode(query),
        num_results,
        api_key
    );

    // Make the request
    let client = reqwest::Client::new();
    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("Search request failed: {}", e))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Search API error: {}",
            response.status()
        ));
    }

    let search_response: SerpApiResponse = response
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to parse search results: {}", e))?;

    // Format results
    let mut result = format!("Search results for: '{}'\n{}\n\n", query, "=".repeat(50));

    if let Some(organic) = search_response.organic_results {
        for (i, item) in organic.iter().enumerate().take(num_results) {
            result.push_str(&format!(
                "{}. {}\n   {}\n   {}\n\n",
                i + 1,
                item.title.as_deref().unwrap_or("No title"),
                item.link.as_deref().unwrap_or("No link"),
                item.snippet.as_deref().unwrap_or("No description")
            ));
        }
    } else {
        result.push_str("No results found.\n");
    }

    if let Some(answer_box) = search_response.answer_box {
        if let Some(answer) = answer_box.answer {
            result.push_str(&format!("\nDirect Answer:\n{}\n", answer));
        }
        if let Some(snippet) = answer_box.snippet {
            result.push_str(&format!("\nFeatured Snippet:\n{}\n", snippet));
        }
    }

    Ok(result)
}

#[derive(Debug, Deserialize)]
struct SerpApiResponse {
    organic_results: Option<Vec<OrganicResult>>,
    answer_box: Option<AnswerBox>,
}

#[derive(Debug, Deserialize)]
struct OrganicResult {
    title: Option<String>,
    link: Option<String>,
    snippet: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AnswerBox {
    answer: Option<String>,
    snippet: Option<String>,
}

// Simple URL encoding helper
mod urlencoding {
    pub fn encode(s: &str) -> String {
        let mut result = String::new();
        for c in s.chars() {
            match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                    result.push(c);
                }
                ' ' => result.push_str("%20"),
                _ => {
                    for byte in c.to_string().as_bytes() {
                        result.push_str(&format!("%{:02X}", byte));
                    }
                }
            }
        }
        result
    }
}
