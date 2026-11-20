import { address } from "@solana/kit";
import { describe, expect, it } from "vitest";
import { initializePoseidon } from "../src/hasher/index.js";
import { ShieldedKeypair, randomBlinding } from "../src/keypair/index.js";
import {
  ConfidentialTransfer,
  ProofInputUtxo,
  Utxo,
  SOL_MINT,
  WithdrawalTarget,
} from "../src/transaction/index.js";

const payer = address("4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi");
const recipient = address("11111111111111111111111111111112");
await initializePoseidon();
function builder(amount = 100n) {
  const owner = ShieldedKeypair.generate();
  const note = ProofInputUtxo.fromKeypair(
    new Utxo({
      owner: owner.signingPublicKey(),
      asset: SOL_MINT,
      amount,
      blinding: randomBlinding(),
    }),
    owner,
    {},
    0,
  );
  return new ConfidentialTransfer(owner.shieldedAddress(), [note], payer);
}
const leg = (amount: bigint) => ({
  asset: SOL_MINT,
  amount,
  target: WithdrawalTarget.sol({ recipient }),
});

describe("ordered public withdrawal vector", () => {
  it("aggregates two SOL legs into one asset balance while preserving public destinations", () => {
    const transfer = builder();
    transfer.withdrawMany([
      leg(20n),
      { ...leg(30n), target: WithdrawalTarget.sol({ recipient: payer }) },
    ]);
    const prepared = transfer.prepare();
    expect(prepared.interfaceTransfers).toEqual([
      { kind: "sol", isDeposit: false, amount: 20n, userSolAccount: recipient },
      { kind: "sol", isDeposit: false, amount: 30n, userSolAccount: payer },
    ]);
    expect(
      prepared.outputs.filter((output) => output.amount > 0n).map((output) => output.amount),
    ).toEqual([50n]);
  });
  it("validates the whole batch before mutation and keeps the single-withdrawal guard", () => {
    const transfer = builder();
    expect(() => transfer.withdrawMany([leg(20n), leg(0n)])).toThrow("TRANSACTION_INVALID_AMOUNT");
    expect(() => transfer.withdrawMany([leg(1n << 64n)])).toThrow();
    transfer.withdraw(SOL_MINT, 20n, WithdrawalTarget.sol({ recipient }));
    expect(() => transfer.withdrawMany([leg(30n)])).toThrow("TRANSACTION_WITHDRAWAL_ALREADY_SET");
    expect(transfer.prepare().interfaceTransfers).toHaveLength(1);
  });
  it("snapshots target and amount values and conserves the aggregate balance", () => {
    const transfer = builder(),
      request = leg(20n);
    transfer.withdrawMany([request]);
    request.amount = 90n;
    expect(transfer.prepare().outputs.find((output) => output.amount > 0n)?.amount).toBe(80n);
    const insufficient = builder();
    insufficient.withdrawMany([leg(60n), leg(60n)]);
    expect(() => insufficient.prepare()).toThrow("TRANSACTION_INSUFFICIENT_BALANCE");
  });
});
