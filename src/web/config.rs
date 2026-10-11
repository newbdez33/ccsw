use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

#[derive(Clone)]
pub struct Config {
    pub origin: String,
    pub authority: String,
    pub secure: bool,
    pub read_only: bool,
    pub host: String,
}

impl Config {
    pub fn new(
        bind: SocketAddr,
        external_origin: Option<&str>,
        read_only: bool,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !bind.ip().is_unspecified(),
            "Wildcard listeners are not supported."
        );
        let origin = match external_origin {
            Some(origin) => {
                anyhow::ensure!(
                    bind.ip().is_loopback(),
                    "An external origin requires a loopback listener."
                );
                let url = reqwest::Url::parse(origin)?;
                anyhow::ensure!(
                    url.scheme() == "https"
                        && url.host_str().is_some()
                        && url.username().is_empty()
                        && url.password().is_none()
                        && url.path() == "/"
                        && url.query().is_none()
                        && url.fragment().is_none(),
                    "The external origin must be an HTTPS origin without a path, query, or credentials."
                );
                url.origin().ascii_serialization()
            }
            None => format!("http://{bind}"),
        };
        let authority = origin
            .split_once("://")
            .expect("validated origin")
            .1
            .to_string();
        let host = std::process::Command::new("hostname")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|name| {
                name.trim()
                    .chars()
                    .filter(|c| !c.is_control())
                    .take(96)
                    .collect()
            })
            .unwrap_or_else(|| "ccsw host".to_string());
        Ok(Self {
            secure: origin.starts_with("https:"),
            origin,
            authority,
            read_only,
            host,
        })
    }
}

pub async fn validate_bind(ip: IpAddr) -> anyhow::Result<()> {
    anyhow::ensure!(
        !ip.is_unspecified(),
        "Wildcard listeners are not supported."
    );
    if ip.is_loopback() {
        return Ok(());
    }
    anyhow::ensure!(
        tailscale_addresses().await?.contains(&ip),
        "The listener must use loopback or this host's current Tailscale address."
    );
    Ok(())
}

pub async fn tailscale_addresses() -> anyhow::Result<Vec<IpAddr>> {
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::process::Command::new("tailscale")
            .args(["status", "--json"])
            .kill_on_drop(true)
            .output(),
    )
    .await??;
    anyhow::ensure!(
        output.status.success(),
        "Cannot verify the local Tailscale address."
    );
    let status: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        status["BackendState"] == "Running",
        "Tailscale must be running."
    );
    let local: Vec<IpAddr> = status
        .pointer("/Self/TailscaleIPs")
        .and_then(|v| v.as_array())
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.parse().ok())
        .filter(|ip: &IpAddr| !ip.is_unspecified() && !ip.is_loopback())
        .collect();
    anyhow::ensure!(!local.is_empty(), "Tailscale has no local address.");
    Ok(local)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_explicit_and_ipv6_is_supported() {
        let config = Config::new("[::1]:3000".parse().unwrap(), None, false).unwrap();
        assert_eq!(config.authority, "[::1]:3000");
        assert!(Config::new("0.0.0.0:3000".parse().unwrap(), None, false).is_err());
        for origin in [
            "http://host",
            "https://user@host",
            "https://host/path",
            "https://host/?q=x",
        ] {
            assert!(Config::new("127.0.0.1:3000".parse().unwrap(), Some(origin), false).is_err());
        }
        let config = Config::new(
            "127.0.0.1:3000".parse().unwrap(),
            Some("https://host.example/"),
            true,
        )
        .unwrap();
        assert!(config.secure && config.read_only);
        assert_eq!(config.authority, "host.example");
    }
}
