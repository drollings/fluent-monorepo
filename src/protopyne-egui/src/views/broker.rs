//! Read-only broker panel: venues, accounts, orders, and deals.
//!
//! Reads only. The panel never submits, cancels, or arms; an unsupported
//! surface renders the gateway's refusal instead of an empty table.

use protopyne_view::{
    BrokerAccountView, BrokerCapabilityView, BrokerDealView, BrokerOrderView, GatewayError,
    TerminalGateway,
};

/// One broker-panel pull.
#[derive(Debug, Clone, Default, PartialEq)]
/// One broker-panel pull.
pub struct BrokerModel {
    /// Venues and their declared capabilities.
    pub venues: Vec<BrokerCapabilityView>,
    /// Account summaries.
    pub accounts: Vec<BrokerAccountView>,
    /// Orders for the selected account.
    pub orders: Vec<BrokerOrderView>,
    /// Deals for the selected account.
    pub deals: Vec<BrokerDealView>,
    /// The account whose orders/deals were pulled, if any.
    pub selected: Option<String>,
    /// Refusal line, when a read failed.
    pub refusal: Option<String>,
}

impl BrokerModel {
    /// Pull venues and accounts. Order/deal reads stay empty until
    /// [`select`](Self::select) is called.
    pub fn load(gateway: &dyn TerminalGateway) -> Self {
        let venues = match gateway.broker_venues() {
            Ok(venues) => venues,
            Err(error) => {
                return Self {
                    refusal: Some(error.to_string()),
                    ..Self::default()
                };
            }
        };
        let accounts = match gateway.broker_accounts() {
            Ok(accounts) => accounts,
            Err(error) => {
                return Self {
                    venues,
                    refusal: Some(error.to_string()),
                    ..Self::default()
                };
            }
        };
        Self {
            venues,
            accounts,
            ..Self::default()
        }
    }

    /// Select one account and pull its orders and deals. A failed read keeps
    /// the selection and records the refusal rather than clearing data.
    pub fn select(&mut self, gateway: &dyn TerminalGateway, account: &str) {
        self.selected = Some(account.to_owned());
        self.orders.clear();
        self.deals.clear();
        match gateway.broker_orders(account) {
            Ok(orders) => self.orders = orders,
            Err(error) => self.refusal = Some(error.to_string()),
        }
        match gateway.broker_deals(account) {
            Ok(deals) => self.deals = deals,
            Err(error) => self.refusal = Some(error.to_string()),
        }
    }

    /// Whether any venue advertises a surface.
    #[must_use]
    pub fn supports(&self, surface: &str) -> bool {
        self.venues.iter().any(|venue| match surface {
            "streams" => venue.streams,
            "accounts" => venue.accounts,
            "orders" => venue.orders,
            "deals" => venue.deals,
            "auth_refresh" => venue.auth_refresh,
            "native_bars" => venue.native_bars,
            _ => false,
        })
    }

    /// Pull with every account selected in turn (read-only).
    pub fn load_full(gateway: &dyn TerminalGateway) -> Self {
        let mut model = Self::load(gateway);
        if let Some(account) = model
            .accounts
            .first()
            .map(|account| account.account.clone())
        {
            model.select(gateway, &account);
        }
        model
    }
}

/// Render the panel body. Infallible: every refusal is text, never a panic.
pub fn paint_broker_panel(ui: &mut egui::Ui, model: &BrokerModel) {
    if let Some(refusal) = &model.refusal {
        ui.colored_label(egui::Color32::RED, refusal);
        return;
    }
    if model.venues.is_empty() {
        ui.label("No broker connected");
        return;
    }
    for venue in &model.venues {
        ui.label(format!(
            "{}  streams={} accounts={} orders={} deals={} auth={} bars={}",
            venue.venue,
            venue.streams,
            venue.accounts,
            venue.orders,
            venue.deals,
            venue.auth_refresh,
            venue.native_bars
        ));
    }
    for account in &model.accounts {
        ui.label(format!(
            "{} {}  net={} buying_power={} margin={}",
            account.venue,
            account.account,
            account.net_liquid,
            account.buying_power,
            account.margin_used
        ));
    }
    if let Some(selected) = &model.selected {
        ui.separator();
        ui.label(format!("orders ({selected})"));
        for order in &model.orders {
            ui.label(format!(
                "{} {} {} filled {}/{} limit {}",
                order.client_id,
                order.status,
                order.underlying,
                order.filled_quantity,
                order.quantity,
                order.limit_price
            ));
        }
        ui.label(format!("deals ({selected})"));
        for deal in &model.deals {
            ui.label(format!(
                "{} {} {} @ {} fee {}",
                deal.deal_id, deal.underlying, deal.quantity, deal.price, deal.fee
            ));
        }
    }
}

/// One refusal from a broker panel read, shaped for tests.
#[must_use]
/// One refusal from a broker panel read, shaped for tests.
pub fn refusal_of(error: &GatewayError) -> String {
    error.to_string()
}

#[cfg(test)]
#[path = "../../tests/broker_view.rs"]
mod tests;