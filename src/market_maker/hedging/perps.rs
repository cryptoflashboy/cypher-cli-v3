use async_trait::async_trait;
use log::info;

use crate::common::{
    context::ExecutionContext,
    hedger::{Hedger, HedgerError, HedgerPulseResult},
    strategy::{Strategy, StrategyError},
};

pub struct PerpsHedger {
    symbol: String,
}

impl PerpsHedger {
    pub fn new(symbol: String) -> Self {
        Self { symbol }
    }
}

impl Hedger for PerpsHedger {
    type Input = ExecutionContext;
    fn pulse(&self, _input: &ExecutionContext) -> Result<HedgerPulseResult, HedgerError> {
        info!("[{}] Running cypher perps hedger logic..", self.symbol);

        Ok(HedgerPulseResult::default())
    }
}

#[async_trait]
impl Strategy for PerpsHedger
where
    Self: Hedger,
{
    type Input = ExecutionContext;
    type Output = HedgerPulseResult;

    async fn execute(&self, ctx: &ExecutionContext) -> Result<HedgerPulseResult, StrategyError> {
        info!("[{}] Running strategy..", self.symbol,);
        self.pulse(ctx);
        Ok(HedgerPulseResult { size_executed: 0 })
    }
}
