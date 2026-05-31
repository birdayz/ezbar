//! Fixed-size ring buffer for graph histories. Port of widget.usageHistory.

#[derive(Debug, Clone)]
pub struct History {
    values: Vec<f64>,
    max_len: usize,
    pos: usize,
}

impl History {
    pub fn new(max_len: usize) -> Self {
        History {
            values: vec![0.0; max_len],
            max_len,
            pos: 0,
        }
    }

    pub fn add(&mut self, value: f64) {
        self.values[self.pos] = value;
        self.pos = (self.pos + 1) % self.max_len;
    }

    /// Returns values in chronological order (oldest first).
    pub fn ordered(&self) -> Vec<f64> {
        (0..self.max_len)
            .map(|i| self.values[(self.pos + i) % self.max_len])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_orders_oldest_first_and_wraps() {
        let mut h = History::new(3);
        assert_eq!(h.ordered(), vec![0.0, 0.0, 0.0]);
        h.add(1.0);
        h.add(2.0);
        h.add(3.0);
        assert_eq!(h.ordered(), vec![1.0, 2.0, 3.0]);
        h.add(4.0); // wraps, oldest dropped
        assert_eq!(h.ordered(), vec![2.0, 3.0, 4.0]);
        h.add(5.0);
        assert_eq!(h.ordered(), vec![3.0, 4.0, 5.0]);
    }
}
