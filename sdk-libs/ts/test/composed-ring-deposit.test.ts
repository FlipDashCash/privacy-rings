/**
 * Exact SDK serialization, not a proof or transaction. An in-memory transport always refuses.
 */
import { address, type Address } from "@solana/kit";
import { beforeAll, describe, expect, it } from "vitest";

import { assemble, prepareTransfer } from "../src/client/prover/assembly.js";
import { ProverClient } from "../src/client/prover/client.js";
import { decodeIndexedInputs } from "../src/client/prover/indexed.js";
import { initializePoseidon } from "../src/hasher/index.js";
import { InstructionTag } from "../src/interface/program.js";
import { randomSalt } from "../src/keypair/bytes.js";
import { ShieldedKeypair } from "../src/keypair/index.js";
import { encodeConfidentialSlots } from "../src/transaction/instructions/transact.js";
import {
  AssetRegistry,
  ConfidentialTransfer,
  ProofInputUtxo,
  SOL_MINT,
  WithdrawalTarget,
  type DepositSource,
} from "../src/transaction/index.js";

const payer = address("4vJ9JU1bJJE96FWSJKvHsmmFADCg4gpZQff4P3bkLKi");
const ring = address("8qbHbw2BbbTHBW1sbeqakYXVKRQM8Ne7pLK7m6CVfeR");
const funder = address("GgBaCs3NCBuZN12kCJgAW63ydqohFkHEdfdEXBPzLHq");
const mint = address("CktRuQ2mttgRGkXJtyksdKHjUdc2C4TgDzyB98oEzy8");
const treeId = 7;
const amount = 25n;

beforeAll(() => initializePoseidon());

function frame() {
  const sender = ShieldedKeypair.generate();
  const recipient = ShieldedKeypair.generate().shieldedAddress();
  const transfer = ConfidentialTransfer.newDeposit(
    sender.shieldedAddress(),
    treeId,
    payer,
  ).withRingProgramId(ring);
  return { sender, recipient, transfer };
}

function seal(transfer: ConfidentialTransfer, sender: ShieldedKeypair, assets: AssetRegistry) {
  const prepared = transfer.prepare();
  const view = sender.viewingKey();
  const tx = view.transactionViewingKey(prepared.firstNullifier);
  try {
    const salt = randomSalt();
    return prepared.finalize({
      txViewingPublicKey: tx.publicKey(),
      salt,
      payload: encodeConfidentialSlots(prepared.outputs, assets, tx, salt),
      instructionDiscriminator: InstructionTag.ringTransact,
    });
  } finally {
    tx.destroy();
    view.destroy();
  }
}

describe("composed custom-ring deposit", () => {
  it.each([
    { asset: SOL_MINT, source: { kind: "sol", funder } },
    { asset: mint, source: { kind: "spl", sourceTokenAccount: funder, splInterfaceBump: 251 } },
  ] satisfies readonly { asset: Address; source: DepositSource }[])(
    "encrypts exactly the funded $source.kind output and sends only the indexed ABI",
    async ({ asset, source }) => {
      const { sender, recipient, transfer } = frame();
      transfer.deposit(asset, amount, source);
      transfer.send(recipient, asset, amount);
      const proofInputs = seal(
        transfer,
        sender,
        new AssetRegistry(asset === SOL_MINT ? [] : [[2n, mint]]),
      );
      expect(
        proofInputs.inputUtxos.every((input) => input.isDummy() && input.treeId === treeId),
      ).toBe(true);
      expect(proofInputs.inputUtxos[0]?.isCompact()).toBe(false);
      expect(proofInputs.outputTreeId).toBe(treeId);
      const paid = proofInputs.outputs.filter((output) => !output.isDummy() && output.amount > 0n);
      expect(paid).toHaveLength(1);
      expect(paid[0]).toMatchObject({ asset, amount, ringProgramId: ring });
      expect(paid[0]?.ownerAddress?.signingPublicKey.toBytes()).toEqual(
        recipient.signingPublicKey.toBytes(),
      );
      expect(proofInputs.externalData.interfaceTransfers).toEqual([
        source.kind === "sol"
          ? { kind: "sol", isDeposit: true, amount, userSolAccount: funder }
          : {
              kind: "spl",
              mint: asset,
              isDeposit: true,
              amount,
              tokenAccount: funder,
              splInterfaceBump: 251,
            },
      ]);

      expect(() => assemble(proofInputs, [], [], { kind: "ring", ring })).toThrow(
        "CLIENT_NO_INPUTS",
      );
      expect(() => prepareTransfer(proofInputs)).toThrow("CLIENT_NO_INPUTS");
      const indexed = decodeIndexedInputs(
        prepareTransfer(proofInputs, { kind: "ring", ring }).inputs,
      );
      expect(indexed.circuit).toBe("transferRing");
      if (indexed.circuit !== "transferRing") throw Error("wrong circuit");
      expect(indexed.trees.map((tree) => tree.id)).toEqual([treeId]);
      expect(indexed.lookups[0]).toEqual({ treeSlot: 0, nullifier: null, commitment: null });
      expect(indexed.payload.publicAmounts.reduce<bigint>((sum, value) => sum + value, 0n)).toBe(
        amount,
      );

      const requests: { url: string; body: unknown }[] = [];
      const fetch: typeof globalThis.fetch = async (url, init) => {
        requests.push({
          url: url instanceof Request ? url.url : String(url),
          body: JSON.parse(String(init?.body)) as unknown,
        });
        return new Response(JSON.stringify({ error: "offline test: no prover configured" }), {
          status: 400,
          headers: { "content-type": "application/json" },
        });
      };
      const prover = new ProverClient({ url: "http://127.0.0.1:3001", fetch });
      await expect(prover.proveIndexed(indexed)).rejects.toBeInstanceOf(Error);
      expect(requests).toEqual([
        {
          url: expect.stringMatching(/\/prove\/transfer_ring_1_2\/indexed$/u),
          body: expect.objectContaining({
            circuitType: "transfer-ring",
            trees: indexed.trees,
            inputs: indexed.lookups,
          }),
        },
      ]);
    },
  );

  it("does not admit an unfunded deposit, mismatched source, second funding leg or withdrawal", () => {
    const { sender, recipient, transfer } = frame();
    expect(
      () =>
        new ConfidentialTransfer(
          sender.shieldedAddress(),
          [ProofInputUtxo.dummy(undefined, treeId)],
          payer,
        ),
    ).toThrow("TRANSACTION_DUMMY_INPUT_NOT_ALLOWED");
    expect(() => transfer.prepare()).toThrow("TRANSACTION_INVALID_DEPOSIT_FRAME");
    expect(() => transfer.deposit(mint, amount, { kind: "sol", funder })).toThrow(
      "TRANSACTION_DEPOSIT_ASSET_MISMATCH",
    );
    expect(() => transfer.deposit(SOL_MINT, 0n, { kind: "sol", funder })).toThrow(
      "TRANSACTION_ZERO_INTERFACE_TRANSFER_AMOUNT",
    );
    transfer.deposit(SOL_MINT, amount, { kind: "sol", funder });
    expect(() => transfer.deposit(SOL_MINT, amount, { kind: "sol", funder })).toThrow(
      "TRANSACTION_INVALID_DEPOSIT_FRAME",
    );
    expect(() =>
      transfer.withdraw(SOL_MINT, 1n, WithdrawalTarget.sol({ recipient: funder })),
    ).toThrow("TRANSACTION_INVALID_DEPOSIT_FRAME");
    transfer.send(recipient, SOL_MINT, amount + 1n);
    expect(() => transfer.prepare()).toThrow("TRANSACTION_INSUFFICIENT_BALANCE");
  });
});
