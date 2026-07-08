use poseidon::poseidon::bn254::hash_2;

fn main(leaf: Field, index: Field, siblings: [Field; 2], root: pub Field) {
    let index_bits: [bool; 2] = index.to_le_bits();
    let mut current = leaf;
    for i in 0..2 {
        if index_bits[i] == false {
            current = hash_2([current, siblings[i]]);
        } else {
            current = hash_2([siblings[i], current]);
        }
    }
    assert(current == root);
}

#[test]
fn print_root() {
    let leaf = 111;
    let sib0 = 222;
    let sib1 = 333;

    let level1 = hash_2([leaf as Field, sib0 as Field]);
    let root = hash_2([level1, sib1 as Field]);

    println(f"leaf={leaf}");
    println(f"sib0={sib0} sib1={sib1}");
    println(f"root={root}");
}
