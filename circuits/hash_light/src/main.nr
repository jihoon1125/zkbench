// Balance threshold proof.
// (Noir only allows ASCII in comments, so this file is in English.)
//
// Minimal payment / stablecoin example: prove "my balance is at or above
// some threshold" WITHOUT revealing the actual balance amount.
//
// - balance   : private input. Only the prover knows the real balance.
// - threshold : pub input. Public reference value (e.g. minimum payment).
//
// The proof only exists if the assert passes, so the mere existence of a
// valid proof means "balance >= threshold" is true.
use poseidon::poseidon::bn254::hash_2;

global ROUNDS: u32 = 10;

fn main(balance: u64, threshold: pub u64) -> pub Field {
    let mut acc = balance as Field;
    for _ in 0..ROUNDS {
        acc = hash_2([acc, threshold as Field]);
    }
    assert(balance >= threshold);
    acc
}
