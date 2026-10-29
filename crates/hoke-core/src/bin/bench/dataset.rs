use hoke_core::BatchOperation;

use super::config::Config;

pub(super) struct Dataset {
    pub(super) keys: Vec<Vec<u8>>,
    pub(super) missing: Vec<Vec<u8>>,
    pub(super) values: Vec<Vec<u8>>,
    pub(super) order: Vec<usize>,
    pub(super) batches: Vec<Vec<BatchOperation>>,
}

impl Dataset {
    pub(super) fn prepare(c: &Config) -> Self {
        let keys: Vec<Vec<u8>> = (0..c.keys)
            .map(|i| {
                let mut key = vec![0x55; c.key_bytes];
                key[..8].copy_from_slice(&(i as u64).to_be_bytes());
                key
            })
            .collect();
        let missing: Vec<Vec<u8>> = (0..c.keys)
            .map(|i| {
                let mut key = keys[i].clone();
                key[..8].copy_from_slice(&((i + c.keys) as u64).to_be_bytes());
                key
            })
            .collect();
        let values: Vec<Vec<u8>> = (0..c.keys)
            .map(|i| {
                (0..c.value_bytes)
                    .map(|j| (i.wrapping_mul(31).wrapping_add(j) % 251) as u8)
                    .collect()
            })
            .collect();
        let mut order: Vec<usize> = (0..c.keys).collect();
        let mut state = c.seed;
        for i in (1..order.len()).rev() {
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
            z ^= z >> 31;
            order.swap(i, (z % (i as u64 + 1)) as usize);
        }
        let batches: Vec<Vec<BatchOperation>> = keys
            .chunks(c.batch_size)
            .zip(values.chunks(c.batch_size))
            .map(|(ks, vs)| {
                ks.iter()
                    .zip(vs)
                    .map(|(key, value)| BatchOperation::Put {
                        key: key.clone(),
                        value: value.clone(),
                    })
                    .collect()
            })
            .collect();
        Self {
            keys,
            missing,
            values,
            order,
            batches,
        }
    }
}
