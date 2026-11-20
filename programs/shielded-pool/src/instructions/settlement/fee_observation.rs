//! Strict observations for the internal fee-aware deposit candidate only.

use pinocchio::{
    error::ProgramError,
    sysvars::clock::{Clock, CLOCK_ID},
    AccountView, Address,
};
use spl_token_2022_interface::{
    extension::{
        transfer_fee::{TransferFee, TransferFeeConfig},
        BaseStateWithExtensions, ExtensionType, PodStateWithExtensions,
    },
    pod::PodMint,
};
use spr_interface::{
    instruction::token_2022_accounting::{EpochDepositView, FeeScheduleEntry, MintFeeProjection},
    SPL_TOKEN_2022_PROGRAM_ID,
};

// Canonical owner of Sysvar accounts, as declared by solana-sdk-ids::sysvar.
const SYSVAR_OWNER: Address = Address::new_from_array([
    6, 167, 213, 23, 24, 117, 247, 41, 199, 61, 147, 64, 143, 33, 97, 32, 6, 126, 216, 140, 118,
    224, 140, 40, 127, 193, 148, 96, 0, 0, 0, 0,
]);

pub(super) fn observe_fee_deposit(
    expected_mint: &Address,
    mint: &AccountView,
    clock: &AccountView,
    gross: u64,
) -> Result<EpochDepositView, ProgramError> {
    let projection = observe_fee_projection(expected_mint, mint, clock)?;
    EpochDepositView::derive(gross, projection).map_err(|_| ProgramError::InvalidArgument)
}

pub(super) fn observe_fee_projection(
    expected_mint: &Address,
    mint: &AccountView,
    clock: &AccountView,
) -> Result<MintFeeProjection, ProgramError> {
    if mint.address() != expected_mint
        || !mint.owned_by(&Address::new_from_array(SPL_TOKEN_2022_PROGRAM_ID))
        || mint.executable()
    {
        return Err(ProgramError::InvalidAccountData);
    }
    if clock.address() != &CLOCK_ID || !clock.owned_by(&SYSVAR_OWNER) || clock.executable() {
        return Err(ProgramError::InvalidAccountData);
    }
    let clock_bytes = clock.try_borrow()?;
    if clock_bytes.len() != Clock::LEN {
        return Err(ProgramError::InvalidAccountData);
    }
    let epoch = Clock::from_bytes(&clock_bytes)?.epoch;

    let mint_bytes = mint.try_borrow()?;
    let extensions = [ExtensionType::TransferFeeConfig];
    let canonical_len = ExtensionType::try_calculate_account_len::<PodMint>(&extensions)?;
    if mint_bytes.len() != canonical_len {
        return Err(ProgramError::InvalidAccountData);
    }
    let state = PodStateWithExtensions::<PodMint>::unpack(&mint_bytes)?;
    if bytemuck::bytes_of(&state.base.is_initialized) != [1]
        || state.get_first_extension_type()? != Some(extensions[0])
    {
        return Err(ProgramError::InvalidAccountData);
    }
    let fee = state.get_extension::<TransferFeeConfig>()?;
    Ok(MintFeeProjection {
        mint: expected_mint.to_bytes().into(),
        observed_epoch: epoch,
        older: projection_entry(&fee.older_transfer_fee),
        newer: projection_entry(&fee.newer_transfer_fee),
    })
}

fn projection_entry(fee: &TransferFee) -> FeeScheduleEntry {
    FeeScheduleEntry {
        epoch: fee.epoch.into(),
        basis_points: fee.transfer_fee_basis_points.into(),
        maximum_fee: fee.maximum_fee.into(),
    }
}

#[cfg(test)]
mod raw_fee_observation {
    use super::*;
    use spl_token_2022_interface::extension::{
        BaseStateWithExtensionsMut, PodStateWithExtensionsMut,
    };
    use spr_account_checks::account_info::test_account_info::get_account_view;

    const MINT: Address = Address::new_from_array([33; 32]);

    fn fixture_mint() -> Vec<u8> {
        let mut bytes = vec![
            0;
            ExtensionType::try_calculate_account_len::<PodMint>(&[
                ExtensionType::TransferFeeConfig,
            ])
            .unwrap()
        ];
        let mut state =
            PodStateWithExtensionsMut::<PodMint>::unpack_uninitialized(&mut bytes).unwrap();
        state.base.is_initialized = true.into();
        let config = state.init_extension::<TransferFeeConfig>(false).unwrap();
        config.older_transfer_fee = TransferFee {
            epoch: 5.into(),
            maximum_fee: 3.into(),
            transfer_fee_basis_points: 25.into(),
        };
        config.newer_transfer_fee = TransferFee {
            epoch: 7.into(),
            maximum_fee: 30.into(),
            transfer_fee_basis_points: 100.into(),
        };
        state.init_account_type().unwrap();
        bytes
    }

    fn fixture_clock(epoch: u64) -> Vec<u8> {
        [41_u64, 1, epoch, 9, 10]
            .into_iter()
            .flat_map(u64::to_le_bytes)
            .collect()
    }

    fn mint_account(bytes: Vec<u8>) -> AccountView {
        get_account_view(
            MINT.to_bytes(),
            SPL_TOKEN_2022_PROGRAM_ID,
            false,
            false,
            false,
            bytes,
        )
    }

    fn clock_account(bytes: Vec<u8>) -> AccountView {
        get_account_view(
            CLOCK_ID.to_bytes(),
            SYSVAR_OWNER.to_bytes(),
            false,
            false,
            false,
            bytes,
        )
    }

    #[test]
    fn reads_real_clock_and_mint_and_switches_epoch_fee() {
        let mint = mint_account(fixture_mint());
        let old = observe_fee_deposit(&MINT, &mint, &clock_account(fixture_clock(6)), 401)
            .unwrap()
            .amounts();
        let new = observe_fee_deposit(&MINT, &mint, &clock_account(fixture_clock(7)), 401)
            .unwrap()
            .amounts();
        assert_eq!((old.gross(), old.net(), old.fee()), (401, 399, 2));
        assert_eq!((new.gross(), new.net(), new.fee()), (401, 396, 5));
    }

    #[test]
    fn refuses_wrong_account_identity_owner_or_executable() {
        let clock = clock_account(fixture_clock(7));
        let mint = mint_account(fixture_mint());
        let bad_owner = get_account_view(
            MINT.to_bytes(),
            [0; 32],
            false,
            false,
            false,
            fixture_mint(),
        );
        let exec_mint = get_account_view(
            MINT.to_bytes(),
            SPL_TOKEN_2022_PROGRAM_ID,
            false,
            false,
            true,
            fixture_mint(),
        );
        let fake_clock = get_account_view(
            CLOCK_ID.to_bytes(),
            [0; 32],
            false,
            false,
            false,
            fixture_clock(7),
        );
        let wrong_address = Address::new_from_array([99; 32]);
        let wrong_clock = get_account_view(
            wrong_address.to_bytes(),
            SYSVAR_OWNER.to_bytes(),
            false,
            false,
            false,
            fixture_clock(7),
        );
        let exec_clock = get_account_view(
            CLOCK_ID.to_bytes(),
            SYSVAR_OWNER.to_bytes(),
            false,
            false,
            true,
            fixture_clock(7),
        );
        for result in [
            observe_fee_projection(&wrong_address, &mint, &clock),
            observe_fee_projection(&MINT, &bad_owner, &clock),
            observe_fee_projection(&MINT, &exec_mint, &clock),
            observe_fee_projection(&MINT, &mint, &fake_clock),
            observe_fee_projection(&MINT, &mint, &wrong_clock),
            observe_fee_projection(&MINT, &mint, &exec_clock),
        ] {
            assert!(result.is_err());
        }
    }

    #[test]
    fn refuses_uninitialized_truncated_overallocated_or_wrong_extension() {
        let clock = clock_account(fixture_clock(7));
        let mut uninitialized = fixture_mint();
        uninitialized[45] = 0;
        let mut wrong_type = fixture_mint();
        wrong_type[166..168].copy_from_slice(&0_u16.to_le_bytes());
        let mut bad_len = fixture_mint();
        bad_len[168..170].copy_from_slice(&0_u16.to_le_bytes());
        let mut truncated = fixture_mint();
        truncated.pop();
        let mut overallocated = fixture_mint();
        overallocated.push(0);
        for bytes in [uninitialized, wrong_type, bad_len, truncated, overallocated] {
            assert!(observe_fee_projection(&MINT, &mint_account(bytes), &clock).is_err());
        }
        let mint = mint_account(fixture_mint());
        for bytes in [vec![0; Clock::LEN - 1], vec![0; Clock::LEN + 1]] {
            assert!(observe_fee_projection(&MINT, &mint, &clock_account(bytes)).is_err());
        }
    }

    #[test]
    fn matches_locked_spl_fee_arithmetic_over_boundary_grid() {
        assert_eq!(
            "Sysvar1111111111111111111111111111111111111"
                .parse::<Address>()
                .unwrap(),
            SYSVAR_OWNER
        );
        let serialized_mint = fixture_mint();
        let raw = mint_account(serialized_mint.clone());
        let spl = PodStateWithExtensions::<PodMint>::unpack(&serialized_mint).unwrap();
        let config = spl.get_extension::<TransferFeeConfig>().unwrap();
        for current_epoch in [5, 6, 7, 8, u64::MAX] {
            let clock = clock_account(fixture_clock(current_epoch));
            for gross in [1, 2, 399, 400, 401, 1_200, 2_999, 3_000, 3_001, u64::MAX] {
                let fee = config.calculate_epoch_fee(current_epoch, gross).unwrap();
                let candidate = observe_fee_deposit(&MINT, &raw, &clock, gross);
                if fee == gross {
                    assert!(candidate.is_err());
                } else {
                    let amounts = candidate.unwrap().amounts();
                    assert_eq!(
                        (amounts.gross(), amounts.net(), amounts.fee()),
                        (gross, gross - fee, fee)
                    );
                }
            }
        }
    }
}
