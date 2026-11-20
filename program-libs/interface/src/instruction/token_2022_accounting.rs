//! Offline settlement accounting primitives. Not a transaction ABI or authorization.

use solana_pubkey::Pubkey;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositAccountingError {
    ZeroNet,
    Overflow,
    ImbalancedTerms,
    InsufficientSource,
    SourceAliasesVault,
    NonCanonicalVault,
    UnexpectedBalances,
    InvalidFeeSchedule,
    StaleFeeView,
}

/// These terms are internally consistent, but do not attest an active fee schedule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositAmounts {
    gross: u64,
    net: u64,
    fee: u64,
}

impl DepositAmounts {
    pub fn checked(gross: u64, net: u64, fee: u64) -> Result<Self, DepositAccountingError> {
        use DepositAccountingError::*;
        if net == 0 {
            return Err(ZeroNet);
        }
        if net.checked_add(fee).ok_or(Overflow)? != gross {
            return Err(ImbalancedTerms);
        }
        Ok(Self { gross, net, fee })
    }

    pub const fn gross(self) -> u64 {
        self.gross
    }

    pub const fn net(self) -> u64 {
        self.net
    }

    pub const fn fee(self) -> u64 {
        self.fee
    }

    /// Verify an immediate deposit-only balance transition, excluding harvest and close.
    /// The caller must authenticate the snapshots and bind them to the same accounts.
    pub fn verify_delta(
        self,
        before: DepositBalances,
        after: DepositBalances,
    ) -> Result<(), DepositAccountingError> {
        use DepositAccountingError::*;
        let expected = DepositBalances {
            source_spendable: before
                .source_spendable
                .checked_sub(self.gross)
                .ok_or(InsufficientSource)?,
            vault_spendable: before
                .vault_spendable
                .checked_add(self.net)
                .ok_or(Overflow)?,
            vault_withheld: before
                .vault_withheld
                .checked_add(self.fee)
                .ok_or(Overflow)?,
            ..before
        };
        if expected != after {
            return Err(UnexpectedBalances);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepositBalances {
    pub source_spendable: u64,
    pub source_withheld: u64,
    pub vault_spendable: u64,
    pub vault_withheld: u64,
    pub mint_withheld: u64,
}

/// Reuse the fork's canonical per-mint derivation, including its checked bump.
/// This does not authenticate account owners, mint contents or the private note recipient.
pub fn verify_deposit_vault(
    mint: &Pubkey,
    source: &Pubkey,
    vault: &Pubkey,
    bump: u8,
) -> Result<(), DepositAccountingError> {
    use DepositAccountingError::*;
    if source == vault {
        return Err(SourceAliasesVault);
    }
    if crate::pda::spl_interface_with_bump(mint) != (*vault, bump) {
        return Err(NonCanonicalVault);
    }
    Ok(())
}

/// Fee terms observed from one schedule entry in the authenticated mint projection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FeeScheduleEntry {
    pub epoch: u64,
    pub basis_points: u16,
    pub maximum_fee: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MintFeeProjection {
    pub mint: Pubkey,
    pub observed_epoch: u64,
    pub older: FeeScheduleEntry,
    pub newer: FeeScheduleEntry,
}

/// No wire representation. The fee is derived from the mint projection, not
/// supplied by an approval or the caller's expected-fee term. Callers must
/// authenticate the mint data, token program and Clock before constructing the
/// projection, and verify the mint's freshness before use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EpochDepositView {
    projection: MintFeeProjection,
    active: FeeScheduleEntry,
    amounts: DepositAmounts,
}

impl EpochDepositView {
    pub fn derive(
        gross: u64,
        projection: MintFeeProjection,
    ) -> Result<Self, DepositAccountingError> {
        const BPS_SCALE: u128 = 10_000;
        use DepositAccountingError::*;
        if projection.older.epoch > projection.newer.epoch
            || projection.older.epoch > projection.observed_epoch
            || u128::from(projection.older.basis_points) > BPS_SCALE
            || u128::from(projection.newer.basis_points) > BPS_SCALE
        {
            return Err(InvalidFeeSchedule);
        }
        let active = if projection.observed_epoch >= projection.newer.epoch {
            projection.newer
        } else {
            projection.older
        };
        let numerator = u128::from(gross) * u128::from(active.basis_points);
        let rounded = numerator.div_ceil(BPS_SCALE);
        let fee =
            u64::try_from(rounded.min(u128::from(active.maximum_fee))).map_err(|_| Overflow)?;
        let net = gross.checked_sub(fee).ok_or(ImbalancedTerms)?;
        let amounts = DepositAmounts::checked(gross, net, fee)?;
        Ok(Self {
            projection,
            active,
            amounts,
        })
    }

    pub const fn amounts(self) -> DepositAmounts {
        self.amounts
    }

    pub const fn mint(self) -> Pubkey {
        self.projection.mint
    }

    pub const fn epoch(self) -> u64 {
        self.projection.observed_epoch
    }

    pub const fn active_fee(self) -> FeeScheduleEntry {
        self.active
    }

    /// A changed or rolled-over observation needs a fresh authorization even
    /// when the rounded fee and net happen to be identical.
    pub fn revalidate(self, current: MintFeeProjection) -> Result<(), DepositAccountingError> {
        if current != self.projection {
            return Err(DepositAccountingError::StaleFeeView);
        }
        Ok(())
    }
}

/// Internal finalized view; it does not construct a wire instruction or
/// authenticate the account owner, token program, or private note recipient.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundDepositView {
    quote: EpochDepositView,
    source: Pubkey,
    vault: Pubkey,
    vault_bump: u8,
}

impl BoundDepositView {
    pub fn bind(
        quote: EpochDepositView,
        source: Pubkey,
        vault: Pubkey,
        vault_bump: u8,
    ) -> Result<Self, DepositAccountingError> {
        verify_deposit_vault(&quote.mint(), &source, &vault, vault_bump)?;
        Ok(Self {
            quote,
            source,
            vault,
            vault_bump,
        })
    }

    pub const fn quote(self) -> EpochDepositView {
        self.quote
    }

    pub const fn source(self) -> Pubkey {
        self.source
    }

    pub const fn vault(self) -> Pubkey {
        self.vault
    }

    pub const fn vault_bump(self) -> u8 {
        self.vault_bump
    }
}

#[cfg(test)]
mod tests {
    use super::{DepositAccountingError::*, *};

    const BEFORE: DepositBalances = DepositBalances {
        source_spendable: 1000,
        source_withheld: 7,
        vault_spendable: 2000,
        vault_withheld: 11,
        mint_withheld: 13,
    };

    #[test]
    fn exact_terms_and_partial_source_conservation() {
        let amounts = DepositAmounts::checked(400, 396, 4).unwrap();
        assert_eq!(
            (amounts.gross(), amounts.net(), amounts.fee()),
            (400, 396, 4)
        );
        let after = DepositBalances {
            source_spendable: 600,
            vault_spendable: 2396,
            vault_withheld: 15,
            ..BEFORE
        };
        assert_eq!(amounts.verify_delta(BEFORE, after), Ok(()));
        for changed in [
            DepositBalances {
                source_spendable: 604,
                ..after
            },
            DepositBalances {
                source_withheld: 0,
                ..after
            },
            DepositBalances {
                vault_spendable: 2400,
                ..after
            },
            DepositBalances {
                vault_withheld: 11,
                ..after
            },
            DepositBalances {
                mint_withheld: 20,
                ..after
            },
        ] {
            assert_eq!(
                amounts.verify_delta(BEFORE, changed),
                Err(UnexpectedBalances)
            );
        }
    }

    #[test]
    fn amounts_and_delta_fail_closed_on_arithmetic_errors() {
        assert_eq!(DepositAmounts::checked(4, 0, 4), Err(ZeroNet));
        assert_eq!(DepositAmounts::checked(400, 396, 5), Err(ImbalancedTerms));
        assert_eq!(
            DepositAmounts::checked(u64::MAX, u64::MAX, 1),
            Err(Overflow)
        );
        assert!(DepositAmounts::checked(u64::MAX, u64::MAX, 0).is_ok());
        let amounts = DepositAmounts::checked(400, 396, 4).unwrap();
        for before in [
            DepositBalances {
                vault_spendable: u64::MAX,
                ..BEFORE
            },
            DepositBalances {
                vault_withheld: u64::MAX,
                ..BEFORE
            },
        ] {
            assert_eq!(amounts.verify_delta(before, BEFORE), Err(Overflow));
        }
        assert_eq!(
            amounts.verify_delta(
                DepositBalances {
                    source_spendable: 399,
                    ..BEFORE
                },
                BEFORE
            ),
            Err(InsufficientSource)
        );
    }

    #[test]
    fn vault_requires_canonical_mint_address_and_bump_and_distinct_source() {
        let mint = Pubkey::new_from_array([1; 32]);
        let other_mint = Pubkey::new_from_array([2; 32]);
        let source = Pubkey::new_from_array([3; 32]);
        let (vault, bump) = crate::pda::spl_interface_with_bump(&mint);
        assert_eq!(verify_deposit_vault(&mint, &source, &vault, bump), Ok(()));
        assert_eq!(
            verify_deposit_vault(&other_mint, &source, &vault, bump),
            Err(NonCanonicalVault)
        );
        assert_eq!(
            verify_deposit_vault(&mint, &source, &vault, bump.wrapping_add(1)),
            Err(NonCanonicalVault)
        );
        assert_eq!(
            verify_deposit_vault(&mint, &vault, &vault, bump),
            Err(SourceAliasesVault)
        );
    }

    fn fees(observed_epoch: u64) -> MintFeeProjection {
        MintFeeProjection {
            mint: Pubkey::new_from_array([17; 32]),
            observed_epoch,
            older: FeeScheduleEntry {
                epoch: 5,
                basis_points: 25,
                maximum_fee: 3,
            },
            newer: FeeScheduleEntry {
                epoch: 7,
                basis_points: 100,
                maximum_fee: 30,
            },
        }
    }

    #[test]
    fn derives_rounded_capped_fee_at_epoch_boundary() {
        let old = EpochDepositView::derive(401, fees(6)).unwrap();
        let new = EpochDepositView::derive(401, fees(7)).unwrap();
        let capped = EpochDepositView::derive(10_000, fees(7)).unwrap();
        assert_eq!(old.amounts(), DepositAmounts::checked(401, 399, 2).unwrap());
        assert_eq!(new.amounts(), DepositAmounts::checked(401, 396, 5).unwrap());
        assert_eq!(
            capped.amounts(),
            DepositAmounts::checked(10_000, 9_970, 30).unwrap()
        );
        assert_eq!(new.mint(), fees(7).mint);
        assert_eq!(new.epoch(), 7);
        assert_eq!(new.active_fee(), fees(7).newer);
    }

    #[test]
    fn rejects_changed_projection_and_keeps_wide_arithmetic() {
        let p = fees(7);
        let view = EpochDepositView::derive(u64::MAX, p).unwrap();
        assert_eq!(view.amounts().net(), u64::MAX - 30);
        assert_eq!(view.revalidate(p), Ok(()));
        assert_eq!(view.revalidate(fees(8)), Err(StaleFeeView));
        assert_eq!(
            view.revalidate(MintFeeProjection {
                mint: Pubkey::new_from_array([18; 32]),
                ..p
            }),
            Err(StaleFeeView)
        );
        let mut same_rounded_fee = p;
        same_rounded_fee.older.maximum_fee = 4;
        assert_eq!(view.revalidate(same_rounded_fee), Err(StaleFeeView));
    }

    #[test]
    fn fails_closed_on_invalid_schedule_and_zero_net() {
        assert_eq!(
            EpochDepositView::derive(400, fees(4)),
            Err(InvalidFeeSchedule)
        );
        let mut p = fees(7);
        p.newer.basis_points = 10_001;
        assert_eq!(EpochDepositView::derive(400, p), Err(InvalidFeeSchedule));
        p.newer.basis_points = 10_000;
        assert_eq!(EpochDepositView::derive(20, p), Err(ZeroNet));
        p.newer.basis_points = 0;
        assert_eq!(EpochDepositView::derive(20, p).unwrap().amounts().fee(), 0);
        p.newer.epoch = 4;
        assert_eq!(EpochDepositView::derive(20, p), Err(InvalidFeeSchedule));
    }

    #[test]
    fn bound_view_preserves_quote_and_canonical_identities() {
        let quote = EpochDepositView::derive(401, fees(7)).unwrap();
        let source = Pubkey::new_from_array([19; 32]);
        let (vault, bump) = crate::pda::spl_interface_with_bump(&quote.mint());
        let view = BoundDepositView::bind(quote, source, vault, bump).unwrap();
        assert_eq!(
            (view.quote(), view.source(), view.vault(), view.vault_bump()),
            (quote, source, vault, bump)
        );
        assert_eq!(
            BoundDepositView::bind(quote, source, vault, bump.wrapping_add(1)),
            Err(NonCanonicalVault)
        );
        assert_eq!(
            BoundDepositView::bind(quote, vault, vault, bump),
            Err(SourceAliasesVault)
        );
    }
}
