use forge::prelude::*;

/// ISS Location record
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct IssLocation {
    pub id: Uuid,
    pub latitude: f64,
    pub longitude: f64,
    pub api_timestamp: Timestamp,
    pub created_at: Timestamp,
}

#[derive(Debug, serde::Deserialize)]
struct IssApiResponse {
    iss_position: IssPosition,
    timestamp: i64,
    message: String,
}

#[derive(Debug, serde::Deserialize)]
struct IssPosition {
    latitude: String,
    longitude: String,
}

#[derive(Debug, PartialEq)]
struct ParsedIssSnapshot {
    latitude: f64,
    longitude: f64,
    timestamp: i64,
}

fn parse_iss_snapshot(data: IssApiResponse) -> ParsedIssSnapshot {
    ParsedIssSnapshot {
        latitude: data.iss_position.latitude.parse().unwrap_or(0.0),
        longitude: data.iss_position.longitude.parse().unwrap_or(0.0),
        timestamp: data.timestamp,
    }
}

/// Get the latest ISS location from database
#[forge::query(auth = "none")]
pub async fn get_iss_location(ctx: &QueryContext) -> Result<Option<IssLocation>> {
    sqlx::query_as!(
        IssLocation,
        r#"
        SELECT id, latitude, longitude, api_timestamp, created_at
        FROM iss_location
        ORDER BY created_at DESC
        LIMIT 1
        "#
    )
    .fetch_optional(ctx.db())
    .await
    .map_err(Into::into)
}

/// Polls ISS location every minute from Open Notify API
#[forge::cron("* * * * *", timezone = "UTC")]
pub async fn iss_location(ctx: &CronContext) -> Result<()> {
    tracing::info!(run_id = %ctx.run_id, "Fetching ISS location");

    let response = ctx
        .http()
        .get("http://api.open-notify.org/iss-now.json")
        .send()
        .await
        .map_err(|e| ForgeError::internal(format!("HTTP request failed: {}", e)))?;

    if !response.status().is_success() {
        tracing::error!(
            status = response.status().as_u16(),
            "Failed to fetch ISS location"
        );
        return Err(ForgeError::internal("Failed to fetch ISS location"));
    }

    let data: IssApiResponse = response
        .json()
        .await
        .map_err(|e| ForgeError::Deserialization(format!("Failed to parse: {}", e)))?;

    if data.message != "success" {
        tracing::warn!(message = %data.message, "ISS API non-success");
    }

    let snapshot = parse_iss_snapshot(data);

    sqlx::query!(
        "INSERT INTO iss_location (id, latitude, longitude, api_timestamp, created_at) \
         VALUES (gen_random_uuid(), $1, $2, to_timestamp($3), NOW())",
        snapshot.latitude,
        snapshot.longitude,
        snapshot.timestamp as f64
    )
    .execute(ctx.db())
    .await?;

    tracing::debug!(
        latitude = snapshot.latitude,
        longitude = snapshot.longitude,
        "ISS location stored"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_iss_snapshot_preserves_api_values() {
        let snapshot = parse_iss_snapshot(IssApiResponse {
            iss_position: IssPosition {
                latitude: "7.1234".into(),
                longitude: "81.4321".into(),
            },
            timestamp: 1_700_000_000,
            message: "success".into(),
        });

        assert_eq!(
            snapshot,
            ParsedIssSnapshot {
                latitude: 7.1234,
                longitude: 81.4321,
                timestamp: 1_700_000_000,
            }
        );
    }

    #[test]
    fn test_parse_iss_snapshot_falls_back_for_invalid_coordinates() {
        let snapshot = parse_iss_snapshot(IssApiResponse {
            iss_position: IssPosition {
                latitude: "north".into(),
                longitude: "east".into(),
            },
            timestamp: 42,
            message: "failure".into(),
        });

        assert_eq!(snapshot.latitude, 0.0);
        assert_eq!(snapshot.longitude, 0.0);
        assert_eq!(snapshot.timestamp, 42);
    }
}
