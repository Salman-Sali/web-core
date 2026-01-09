use std::time::Duration;

use reqwest::Client;
use serde_json::Value;
use url::Url;

pub async fn post_api<R>(url: &Url, url_path: &str, json_data: impl Into<Value>) -> Result<R, ()>
where
    R: serde::de::DeserializeOwned,
{
    let api_url = match url.join(url_path) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("Error while joining url with path : {}", e);
            return Err(());
        }
    };

    println!("Api call begin : {:?}", &api_url);

    let client = Client::new();
    let response = client
        .post(api_url.clone())
        .json(&json_data.into())
        .timeout(Duration::from_secs(5))
        .send()
        .await;

    println!("Api call response: {:?}", response);

    let response = match response {
        Ok(x) => x,
        Err(e) => {
            eprintln!("Error while calling api : {} : {}", api_url, e);
            return Err(());
        }
    };

    if !response.status().is_success() {
        println!("Api call failed with status code : {}", response.status());
        return Err(());
    }

    return match response.json().await {
        Ok(x) => Ok(x),
        Err(e) => {
            eprintln!("Error while deserialising api resonse : {}", e);
            Err(())
        },
    };
}
