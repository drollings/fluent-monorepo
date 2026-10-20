//! Broker panel view-model: unsupported reads render a refusal, never an
//! empty table.

use protopyne_view::MockGateway;

use crate::views::BrokerModel;

#[test]
fn mock_gateway_refusal_is_shown() {
    let gateway = MockGateway::new();
    let model = BrokerModel::load(&gateway);
    assert!(model.venues.is_empty());
    assert!(model.refusal.is_some(), "unsupported reads refuse");
    assert!(!model.supports("accounts"));
}
