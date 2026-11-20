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
}
