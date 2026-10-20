//! Broker composition tests: substitution, duplicate/unknown ids, and
//! capability refusals.

use std::sync::Arc;

use crate::account::{
    AccountError, AccountFuture, AccountRange, AccountReader, AccountSummary, BrokerDeal,
};
use crate::broker::{BrokerBundle, BrokerCapabilities, BrokerError, BrokerId, BrokerRegistry};
use crate::feed::{ChannelFeed, FeedCapabilities};
use crate::order::OrderRecord;
use crate::paper::{PaperConfig, PaperVenue};

struct FakeAccount;

impl AccountReader for FakeAccount {
    fn capabilities(&self) -> BrokerCapabilities {
        BrokerCapabilities {
            accounts: true,
            orders: true,
            deals: true,
            ..BrokerCapabilities::default()
        }
    }

    fn accounts(&self) -> AccountFuture<'_, Result<Vec<String>, AccountError>> {
        Box::pin(async { Ok(vec!["ACC-1".to_owned()]) })
    }

    fn summary<'a>(
        &'a self,
        account: &'a str,
    ) -> AccountFuture<'a, Result<AccountSummary, AccountError>> {
        Box::pin(async move {
            if account != "ACC-1" {
                return Err(AccountError::Terminal("unknown".to_owned()));
            }
            Ok(AccountSummary {
                account: account.to_owned(),
                venue: "fake".to_owned(),
                currency: "USD".to_owned(),
                net_liquid: protopyne_types::Money::from(1000),
                buying_power: protopyne_types::Money::from(500),
                margin_used: protopyne_types::Money::zero(),
                observed_at: chrono::Utc::now(),
            })
        })
    }

    fn orders<'a>(
        &'a self,
        _account: &'a str,
        _range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn open_orders<'a>(
        &'a self,
        _account: &'a str,
        _range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<OrderRecord>, AccountError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn deals<'a>(
        &'a self,
        _account: &'a str,
        _range: AccountRange,
    ) -> AccountFuture<'a, Result<Vec<BrokerDeal>, AccountError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

fn paper() -> Arc<PaperVenue> {
    Arc::new(PaperVenue::new(PaperConfig::new().build()))
}

#[test]
fn bundles_substitute_through_trait_objects() {
    let id = BrokerId::new("fake").expect("id");
    let bundle = BrokerBundle::new(id.clone(), paper() as Arc<dyn crate::venue::Venue>)
        .with_account(Arc::new(FakeAccount))
        .with_feed(Arc::new(ChannelFeed::new(
            FeedCapabilities {
                quotes: true,
                ..FeedCapabilities::default()
            },
            8,
        )));
    let registry = BrokerRegistry::new();
    registry.register(bundle).expect("registered");
    let found = registry.get(&id).expect("found");
    assert!(found.capabilities.accounts);
    assert!(found.capabilities.orders);
    assert!(found.capabilities.deals);
    assert!(found.capabilities.streams);
    assert!(found.require_account().is_ok());
    assert!(found.require_feed().is_ok());
}

#[test]
fn duplicate_unknown_and_unsupported_refuse() {
    let id = BrokerId::new("fake").expect("id");
    let registry = BrokerRegistry::new();
    registry
        .register(BrokerBundle::new(
            id.clone(),
            paper() as Arc<dyn crate::venue::Venue>,
        ))
        .expect("registered");
    let duplicate = registry.register(BrokerBundle::new(
        id.clone(),
        paper() as Arc<dyn crate::venue::Venue>,
    ));
    assert!(matches!(duplicate, Err(BrokerError::Duplicate(_))));
    let unknown = BrokerId::new("missing").expect("id");
    assert!(matches!(
        registry.get(&unknown),
        Err(BrokerError::Unknown(_))
    ));
    let bundle = registry.get(&id).expect("found");
    let err = bundle
        .require_account()
        .err()
        .expect("execution-only refuses");
    assert!(matches!(err, BrokerError::Unsupported { .. }));
    assert!(err.to_string().contains("account reads"));
}
