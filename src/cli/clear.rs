use crate::cache::CacheDb;
use crate::config::Config;
use anyhow::{bail, Result};

/// Parse a duration string like "30d", "7d", "24h" into seconds.
pub fn parse_duration(s: &str) -> Result<u64> {
    let s = s.trim();
    if s.is_empty() {
        bail!("empty duration string");
    }

    let (num_str, suffix) = s.split_at(s.len() - 1);
    let num: u64 = num_str.parse().map_err(|_| {
        anyhow::anyhow!(
            "invalid duration '{}': expected format like '30d', '7d', or '24h'",
            s
        )
    })?;

    match suffix {
        "d" => Ok(num * 86400),
        "h" => Ok(num * 3600),
        "m" => Ok(num * 60),
        "s" => Ok(num),
        _ => bail!(
            "unknown duration suffix '{}': expected 'd' (days), 'h' (hours), 'm' (minutes), or 's' (seconds)",
            suffix
        ),
    }
}

/// Run the `dja clear` command.
pub async fn run(older_than: Option<String>) -> Result<()> {
    let db_path = Config::data_dir().join("cache.db");

    if !db_path.exists() {
        anyhow::bail!("Cache database not found. Run `dja init` first.");
    }

    let db = CacheDb::open(&db_path).await?;

    let deleted = match older_than {
        Some(duration_str) => {
            let secs = parse_duration(&duration_str)?;
            let deleted = db.evict_by_ttl(secs).await?;
            println!(
                "Deleted {} entries older than {}.",
                deleted, duration_str
            );
            deleted
        }
        None => {
            let deleted = db.clear_all().await?;
            println!("Cleared all {} cache entries.", deleted);
            deleted
        }
    };

    let _ = deleted;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration("30d").unwrap(), 30 * 86400);
        assert_eq!(parse_duration("7d").unwrap(), 7 * 86400);
        assert_eq!(parse_duration("1d").unwrap(), 86400);
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("24h").unwrap(), 24 * 3600);
        assert_eq!(parse_duration("1h").unwrap(), 3600);
        assert_eq!(parse_duration("48h").unwrap(), 48 * 3600);
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("30m").unwrap(), 30 * 60);
        assert_eq!(parse_duration("1m").unwrap(), 60);
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("60s").unwrap(), 60);
        assert_eq!(parse_duration("1s").unwrap(), 1);
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("abc").is_err());
        assert!(parse_duration("30x").is_err());
        assert!(parse_duration("d").is_err());
    }

    #[test]
    fn test_parse_duration_whitespace() {
        assert_eq!(parse_duration(" 30d ").unwrap(), 30 * 86400);
    }
}
