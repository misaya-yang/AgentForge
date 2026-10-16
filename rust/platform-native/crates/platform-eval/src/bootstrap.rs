//! MT19937 integer seeding and rejection sampling matching CPython random.Random(42).
pub(crate) struct Random {
    state: [u32; 624],
    index: usize,
}
impl Random {
    pub(crate) fn new(seed: u32) -> Self {
        let mut r = Self {
            state: [0; 624],
            index: 624,
        };
        r.state[0] = 19650218;
        for i in 1..624 {
            r.state[i] = 1812433253u32
                .wrapping_mul(r.state[i - 1] ^ (r.state[i - 1] >> 30))
                .wrapping_add(i as u32)
        }
        let mut i = 1;
        for _ in 0..624 {
            r.state[i] = (r.state[i]
                ^ ((r.state[i - 1] ^ (r.state[i - 1] >> 30)).wrapping_mul(1664525)))
            .wrapping_add(seed);
            i += 1;
            if i >= 624 {
                r.state[0] = r.state[623];
                i = 1;
            }
        }
        for _ in 0..623 {
            r.state[i] = (r.state[i]
                ^ ((r.state[i - 1] ^ (r.state[i - 1] >> 30)).wrapping_mul(1566083941)))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= 624 {
                r.state[0] = r.state[623];
                i = 1;
            }
        }
        r.state[0] = 0x80000000;
        r
    }
    pub(crate) fn next(&mut self) -> u32 {
        if self.index >= 624 {
            for i in 0..624 {
                let y = (self.state[i] & 0x80000000) | (self.state[(i + 1) % 624] & 0x7fffffff);
                self.state[i] = self.state[(i + 397) % 624]
                    ^ (y >> 1)
                    ^ if y & 1 != 0 { 0x9908b0df } else { 0 };
            }
            self.index = 0
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c5680;
        y ^= (y << 15) & 0xefc60000;
        y ^= y >> 18;
        y
    }
    fn range(&mut self, n: usize) -> usize {
        let bits = usize::BITS - n.leading_zeros();
        loop {
            let value = (self.next() >> (32 - bits)) as usize;
            if value < n {
                return value;
            }
        }
    }
}
pub fn mean_ci(values: &[f64], samples: usize) -> Option<[f64; 2]> {
    if values.is_empty() {
        return None;
    }
    let mut rng = Random::new(42);
    let mut means = (0..samples)
        .map(|_| {
            (0..values.len())
                .map(|_| values[rng.range(values.len())])
                .sum::<f64>()
                / values.len() as f64
        })
        .collect::<Vec<_>>();
    means.sort_by(f64::total_cmp);
    let percentile = |q: f64| {
        let p = (means.len() - 1) as f64 * q;
        let l = p.floor() as usize;
        let u = p.ceil() as usize;
        means[l] + (means[u] - means[l]) * (p - l as f64)
    };
    Some([percentile(0.025), percentile(0.975)])
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn python_seed_42() {
        let mut r = Random::new(42);
        assert_eq!(
            (0..5).map(|_| r.next()).collect::<Vec<_>>(),
            vec![2746317213, 478163327, 107420369, 3184935163, 1181241943]
        );
    }
    #[test]
    fn constant_ci() {
        assert_eq!(mean_ci(&[1.; 8], 2000), Some([1., 1.]));
    }
}
