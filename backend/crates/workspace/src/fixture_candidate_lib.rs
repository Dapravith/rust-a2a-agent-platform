//! Fixture: a request payload for creating a campaign, as an Axum handler
//! would receive it after JSON deserialization.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateCampaign {
    pub title: String,
    pub goal_cents: u64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyTitle,
    UnsupportedCurrency,
}

const CURRENCIES: &[&str] = &["USD", "EUR", "GBP"];

/// Validates the payload. Existing behaviour that callers rely on:
/// only an empty title is rejected.
pub fn validate(p: &CreateCampaign) -> Result<(), ValidationError> {
    if p.title.trim().is_empty() {
        return Err(ValidationError::EmptyTitle);
    }
    if !CURRENCIES.contains(&p.currency.as_str()) {
        return Err(ValidationError::UnsupportedCurrency);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> CreateCampaign {
        CreateCampaign {
            title: "Library roof".into(),
            goal_cents: 500_000,
            currency: "USD".into(),
        }
    }

    #[test]
    fn accepts_valid_payload() {
        assert_eq!(validate(&ok()), Ok(()));
    }

    #[test]
    fn rejects_empty_title() {
        let mut p = ok();
        p.title = "  ".into();
        assert_eq!(validate(&p), Err(ValidationError::EmptyTitle));
    }

    #[test]
    fn rejects_unknown_currency() {
        let mut p = ok();
        p.currency = "XYZ".into();
        assert_eq!(validate(&p), Err(ValidationError::UnsupportedCurrency));
    }
}
