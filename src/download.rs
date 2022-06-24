use crate::{SkynetClient, SkynetError::*, SkynetResult, util::make_uri, URI_SKYNET_PREFIX};
use std::{
  collections::HashMap,
  fs,
  path::Path,
  io::Write,
  str,
};
use hyper::{body, Body, Request};
use mime::Mime;
use serde_json::Value as Json;
use futures::{Stream, StreamExt};

pub type BytesDownloaded = u64;

#[derive(Debug)]
pub struct DownloadOptions {
  pub endpoint_path: String,
  pub api_key: Option<String>,
  pub custom_user_agent: Option<String>,
  pub skykey_name: Option<String>,
  pub skykey_id: Option<String>,
}

impl Default for DownloadOptions {
  fn default() -> Self {
    Self {
      endpoint_path: "/".to_string(),
      api_key: None,
      custom_user_agent: None,
      skykey_name: None,
      skykey_id: None,
    }
  }
}

#[derive(Debug)]
pub struct MetadataOptions {
  pub endpoint_path: String,
  pub api_key: Option<String>,
  pub custom_user_agent: Option<String>,
}

impl Default for MetadataOptions {
  fn default() -> Self {
    Self {
      endpoint_path: "/".to_string(),
      api_key: None,
      custom_user_agent: None,
    }
  }
}

pub async fn download_data(
  client: &SkynetClient,
  skylink: &str,
  opt: DownloadOptions,
) -> SkynetResult<Vec<u8>> {
  let req = Request::builder().method("GET");

  let mut query = HashMap::new();

  let skylink = if skylink.starts_with(URI_SKYNET_PREFIX) {
    &skylink[URI_SKYNET_PREFIX.len()..]
  } else {
    skylink
  };

  if let Some(ref skykey_name) = opt.skykey_name {
    query.insert("skykeyname".into(), skykey_name.clone());
  }

  if let Some(ref skykey_id) = opt.skykey_id {
    query.insert("skykeyid".into(), skykey_id.clone());
  }

  let uri = make_uri(
    client.get_portal_url(),
    opt.endpoint_path,
    opt.api_key,
    Some(skylink.to_string()),
    query);

  let mut req = req.uri(uri);

  if let Some(custom_user_agent) = opt.custom_user_agent {
    req = req.header("User-Agent", custom_user_agent);
  }

  let req = req.body(Body::from("")).map_err(HttpError)?;
  let res = client.http.request(req).await.map_err(HyperError)?;
  let body = body::to_bytes(res.into_body()).await.map_err(HyperError)?;

  Ok(body.to_vec())
}

pub async fn download_file<P: AsRef<Path>>(
  client: &SkynetClient,
  path: P,
  skylink: &str,
  opt: DownloadOptions,
) -> SkynetResult<()> {
  let data = download_data(client, skylink, opt).await?;
  fs::write(path, data).map_err(FileError)?;

  Ok(())
}

pub fn download_file_stream<P: AsRef<Path>>(
  client: &SkynetClient,
  path: P,
  skylink: &str,
  opt: DownloadOptions,
) -> impl Stream<Item = SkynetResult<BytesDownloaded>> {
  let skylink = if skylink.starts_with(URI_SKYNET_PREFIX) {
    &skylink[URI_SKYNET_PREFIX.len()..]
  } else {
    skylink
  };

  let uri = make_uri(
    client.get_portal_url(),
    opt.endpoint_path.clone(),
    opt.api_key.clone(),
    Some(skylink.to_string()),
    Default::default()); // todo: query

  // https://gist.github.com/giuliano-oliveira/4d11d6b3bb003dba3a1b53f43d81b30d
  async_stream::stream! {

    // Reqwest setup
    // TODO: use the hyper instance embedded in the Client instead
    let res = reqwest::Client::new()
        .get(&uri.to_string())
        .header("Skynet-API-key", opt.api_key.unwrap_or_default())
        .send()
        .await
        .map_err(ReqwestError);

    if res.is_err() {
        yield res.map(|_| 0)
    }

    else {
        let res = res.unwrap();

        let total_size = res
            .content_length()
            .ok_or(CustomError(format!("Failed to get content length from '{}'", &uri)))
            .unwrap();

        // download chunks
        let mut file = fs::File::create(&path)
            .map_err(FileError)
            .unwrap();

        let mut downloaded: u64 = 0;
        let mut stream = res.bytes_stream();
        let mut cont = true;

        while let Some(item) = stream.next().await {
            // todo: do not replace original error
            let chunk = item.or(Err(CustomError(format!("Error while downloading file")))).unwrap();

            file
                .write_all(&chunk)
                .map_err(FileError)
                .unwrap();

            let new = std::cmp::min(downloaded + (chunk.len() as u64), total_size);
            let since_last = new - downloaded;

            downloaded = new;

            yield Ok(downloaded)
        }

        // todo: yield error if expected file size doesnt match?
    }
  }
}

#[derive(Debug, PartialEq)]
pub struct Subfile {
  pub filename: String,
  pub length: u32,
  pub content_type: Mime,
}

#[derive(Debug, PartialEq)]
pub struct Metadata {
  pub skylink: String,
  pub portal_url: String,
  pub content_type: Option<Mime>,
  pub filename: Option<String>,
  pub length: Option<u32>,
  pub subfiles: Option<HashMap<String, Subfile>>,
}

pub async fn get_metadata(
  client: &SkynetClient,
  skylink: &str,
  opt: MetadataOptions,
) -> SkynetResult<Metadata> {
  let req = Request::builder().method("HEAD");

  let skylink = if skylink.starts_with(URI_SKYNET_PREFIX) {
    &skylink[URI_SKYNET_PREFIX.len()..]
  } else {
    skylink
  };

  let uri = make_uri(
    client.get_portal_url(),
    opt.endpoint_path,
    opt.api_key,
    Some(skylink.to_string()),
    HashMap::new());

  let mut req = req.uri(uri);

  if let Some(custom_user_agent) = opt.custom_user_agent {
    req = req.header("User-Agent", custom_user_agent);
  }

  let req = req.body(Body::from("")).map_err(HttpError)?;
  let res = client.http.request(req).await.map_err(HyperError)?;
  let headers = res.headers();

  let skylink = if let Some(skylink) = headers.get("skynet-skylink") {
    skylink.to_str().unwrap().to_string()
  } else {
    skylink.to_string()
  };

  let portal_url = if let Some(portal_url) = headers.get("skynet-portal-api") {
    portal_url.to_str().unwrap().to_string()
  } else {
    client.get_portal_url().to_string()
  };

  let content_type = if let Some(content_type) = headers.get("content-type") {
    Some(content_type.to_str().unwrap().parse().unwrap())
  } else {
    None
  };

  let (filename, length, subfiles) = if let Some(metadata) = headers.get("skynet-file-metadata") {
    let metadata: Json = serde_json::from_str(metadata.to_str().unwrap()).unwrap();
    let filename = if let Some(filename) = metadata.get("filename") {
      Some(filename.as_str().unwrap().to_string())
    } else {
      None
    };
    let length = if let Some(length) = metadata.get("length") {
      Some(length.as_u64().unwrap() as u32)
    } else {
      None
    };
    let subfiles = if let Some(subfiles) = metadata.get("subfiles") {
      let mut map = HashMap::new();

      for (filename, subfile) in subfiles.as_object().unwrap() {
        let subfile = Subfile {
          filename: subfile["filename"].as_str().unwrap().to_string(),
          length: subfile["len"].as_u64().unwrap() as u32,
          content_type: subfile["contenttype"].as_str().unwrap().parse().unwrap(),
        };
        map.insert(filename.into(), subfile);
      }

      Some(map)
    } else {
      None
    };

    (filename, length, subfiles)
  } else {
    (None, None, None)
  };

  Ok(Metadata {
    skylink,
    portal_url,
    content_type,
    filename,
    length,
    subfiles,
  })
}

#[cfg(test)]
mod tests {
  use super::*;

  #[tokio::test]
  async fn test_download_data() {
    let client = SkynetClient::default();
    let skylink = "sia://AACi1FJOFAoRyl2YJyVz1yzsYrOfz18yXgnnbxNM0_UDng";
    let res = download_data(&client, skylink, DownloadOptions::default()).await;
    println!("{:?}", res);
    assert!(res.is_ok());
    let data = res.unwrap();
    assert_eq!(str::from_utf8(&data).unwrap(), "hello world");
  }

  #[tokio::test]
  async fn test_download_file() {
    let client = SkynetClient::default();
    let skylink = "sia://AACi1FJOFAoRyl2YJyVz1yzsYrOfz18yXgnnbxNM0_UDng";
    let res = download_file(&client, "tmp2.txt", skylink, DownloadOptions::default()).await;
    println!("{:?}", res);
    assert!(res.is_ok());
    assert_eq!(fs::read_to_string("tmp2.txt").unwrap(), "hello world");
    fs::remove_file("tmp2.txt").unwrap();
  }

  #[tokio::test]
  async fn test_download_file_stream() {
    let client = SkynetClient::default();
    let skylink = "AABC5fIelZsChCGs-fSBRVc5n2BoHc-LAmehPlPRBjIV9w";
    let mut output_event_stream = download_file_stream(&client, "/tmp/tmp2.txt", skylink, DownloadOptions::default());

    futures_util::pin_mut!(output_event_stream);

    while let Some(output_event) = output_event_stream.next().await.transpose().expect("stream error") {
      dbg!(&output_event);
    }

    // todo check file size
    // assert_eq!();

    fs::remove_file("/tmp/tmp2.txt").unwrap();
  }

  #[tokio::test]
  async fn test_get_metadata() {
    let client = SkynetClient::default();
    let skylink = "sia://AACi1FJOFAoRyl2YJyVz1yzsYrOfz18yXgnnbxNM0_UDng";
    let res = get_metadata(&client, skylink, MetadataOptions::default()).await;
    println!("{:?}", res);
    assert!(res.is_ok());

    let metadata = res.unwrap();
    let mut subfiles = HashMap::new();
    subfiles.insert("hello.txt".into(), Subfile {
      filename: "hello.txt".into(),
      length: 11,
      content_type: mime::TEXT_PLAIN,
    });
    assert_eq!(metadata, Metadata {
      skylink: "AACi1FJOFAoRyl2YJyVz1yzsYrOfz18yXgnnbxNM0_UDng".into(),
      portal_url: "https://siasky.net".into(),
      content_type: Some(mime::TEXT_PLAIN),
      filename: Some("hello.txt".into()),
      length: Some(11),
      subfiles: Some(subfiles),
    });
  }
}
