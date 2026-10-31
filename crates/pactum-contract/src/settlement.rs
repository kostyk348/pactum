//! Согласование трейта провайдера расчётов с escrow-движком.
//!
//! Трейт живёт здесь, чтобы внешние реализации (блокчейн-адаптеры)
//! могли зависеть только от этого крейта.

use crate::escrow::{EscrowEngine, SettlementProvider};

impl SettlementProvider for &mut EscrowEngine {
    fn create_task(
        &mut self,
        contract_id: &str,
        principal: &str,
        budget: &str,
    ) -> crate::escrow::SettlementResult<crate::escrow::EscrowState> {
        EscrowEngine::create_task(self, contract_id, principal, budget)
    }
    fn accept_task(
        &mut self,
        contract_id: &str,
        executor: &str,
    ) -> crate::escrow::SettlementResult<crate::escrow::EscrowState> {
        EscrowEngine::accept_task(self, contract_id, executor)
    }
    fn submit_result(
        &mut self,
        contract_id: &str,
        exec_hash: &str,
    ) -> crate::escrow::SettlementResult<crate::escrow::EscrowState> {
        EscrowEngine::submit_result(self, contract_id, exec_hash)
    }
    fn verify_and_release(
        &mut self,
        contract_id: &str,
        success: bool,
    ) -> crate::escrow::SettlementResult<crate::escrow::EscrowState> {
        EscrowEngine::verify_and_release(self, contract_id, success)
    }
    fn slash_on_timeout(
        &mut self,
        contract_id: &str,
        now_secs: u64,
    ) -> crate::escrow::SettlementResult<crate::escrow::EscrowState> {
        EscrowEngine::slash_on_timeout(self, contract_id, now_secs)
    }
    fn state(&self, contract_id: &str) -> Option<crate::escrow::EscrowState> {
        EscrowEngine::state(self, contract_id)
    }
    fn reputation(&self, participant: &str) -> i64 {
        EscrowEngine::reputation(self, participant)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_object_works() {
        let mut engine = EscrowEngine::new();
        engine.register("c-9", None, (90, 10), 10).unwrap();
        let provider: &mut dyn SettlementProvider = &mut engine;
        assert_eq!(
            provider.create_task("c-9", "p1", "1 USDC"),
            Ok(crate::escrow::EscrowState::Initiated)
        );
        assert_eq!(
            provider.state("c-9"),
            Some(crate::escrow::EscrowState::Initiated)
        );
    }
}
