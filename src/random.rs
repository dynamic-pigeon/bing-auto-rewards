/// 循序渐进（类似 LoL 暴击率 PRD）的伪随机触发器
///
/// 使用线性提升概率模型：第 k 次尝试命中概率为 `min(1, k * base_p)`。
/// 通过对 `base_p` 进行二分搜索校准，使得理论期望触发间隔 ≈ n。
pub struct ExpectedNTrigger {
    /// 校准后的线性增长基础概率（不直接等于 1/n）
    base_p: f64,
    /// 当前已经尝试的连续失败次数（下一次尝试序号 = attempts+1）
    attempts: u32,
}

impl ExpectedNTrigger {
    /// 根据期望间隔 n 构造触发器
    pub fn new(n: u32) -> Self {
        if n <= 1 {
            return Self {
                base_p: 1.0,
                attempts: 0,
            };
        }
        let base_p = Self::calibrate_base_p(n as f64);
        Self {
            base_p,
            attempts: 0,
        }
    }

    /// 二分搜索找到合适的 base_p，使得线性增长概率模型的期望触发间隔接近 target_n
    fn calibrate_base_p(target_n: f64) -> f64 {
        // base_p 不能过大（否则很快到 1），也不能过小（否则期望过长）
        let mut low = 0.0_f64;
        let mut high = 1.0 / target_n; // 直接用 1/n 作为上界，会比需要的值偏大
        // 为防止极端情况，将上界再放宽一些
        high = high.max(1.0 / (target_n * 0.5));

        let mut best = high;
        for _ in 0..50 {
            // 足够的迭代次数
            let mid = (low + high) * 0.5;
            let e = Self::expected_interval_linear(mid);
            if (e - target_n).abs() < (Self::expected_interval_linear(best) - target_n).abs() {
                best = mid;
            }
            if e > target_n {
                // 期望过长，需要增大 base_p
                low = mid;
            } else {
                // 期望过短，需要减小 base_p
                high = mid;
            }
        }
        best.max(1e-9)
    }

    /// 计算给定 base_p 下的期望触发间隔（线性增长直到命中或达到概率=1 保证命中）
    fn expected_interval_linear(base_p: f64) -> f64 {
        if base_p <= 0.0 {
            return f64::INFINITY;
        }
        // K = 最晚必定命中的尝试次数（概率达到或超过 1）
        let k_max = (1.0 / base_p).ceil() as u32;
        let mut prod_fail = 1.0; // 累乘前面失败概率
        let mut expectation = 0.0;
        for k in 1..=k_max {
            let p_k = (k as f64 * base_p).min(1.0); // 第 k 次尝试的命中概率（条件概率）
            let trigger_prob_at_k = p_k * prod_fail; // 第 k 次正好命中的概率
            expectation += k as f64 * trigger_prob_at_k;
            prod_fail *= 1.0 - p_k;
            if prod_fail <= 1e-12 {
                break;
            }
        }
        expectation
    }

    /// 调用一次，返回是否触发
    pub fn next(&mut self) -> bool {
        let attempt_number = self.attempts + 1;
        let chance = (attempt_number as f64 * self.base_p).min(1.0);
        let r = rand::random::<f64>();
        let hit = r < chance;
        if hit {
            self.reset();
        } else {
            self.attempts += 1;
        }
        hit
    }

    /// 重置状态（命中后调用）
    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

#[cfg(test)]
mod test {
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    #[test]
    fn test_expected_n_trigger() {
        let p = (1..=100)
            .into_par_iter()
            .map(|i| test_expected_n_trigger_helper(i, 100000))
            .all(|p| p);
        assert!(p);
    }

    fn test_expected_n_trigger_helper(n: u32, trials: u32) -> bool {
        let mut trigger = super::ExpectedNTrigger::new(n);
        let mut total_calls = 0;

        for _ in 0..trials {
            let mut calls = 0;
            loop {
                calls += 1;
                if trigger.next() {
                    break;
                }
            }
            total_calls += calls;
        }

        let average_calls = total_calls as f64 / trials as f64;
        println!(
            "期望触发次数: {}, 实际平均触发次数: {:.2}",
            n, average_calls
        );

        (average_calls - n as f64).abs() < 0.5
    }
}
