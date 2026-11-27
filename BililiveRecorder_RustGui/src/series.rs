use std::collections::VecDeque;

#[derive(Clone, Debug)]
pub struct Sample {
    pub time: f64,
    pub mbps: f64,
    pub disk_mbps: f64,
}

#[derive(Clone)]
pub struct Series {
    data: VecDeque<Sample>,
    capacity: usize,
    sum_mbps: f64,
    sum_disk: f64,
}

impl Series {
    pub fn new(capacity: usize) -> Self {
        Self { data: VecDeque::with_capacity(capacity), capacity, sum_mbps: 0.0, sum_disk: 0.0 }
    }

    /// 加入一个采样点。非有限值（NaN / Infinity）按 0 处理，免得折线图坐标出错。
    pub fn push(&mut self, mut s: Sample) {
        if !s.mbps.is_finite() { s.mbps = 0.0; }
        if !s.disk_mbps.is_finite() { s.disk_mbps = 0.0; }
        if !s.time.is_finite() { s.time = 0.0; }
        if self.data.len() >= self.capacity {
            if let Some(old) = self.data.pop_front() {
                self.sum_mbps -= old.mbps;
                self.sum_disk -= old.disk_mbps;
            }
        }
        self.sum_mbps += s.mbps;
        self.sum_disk += s.disk_mbps;
        self.data.push_back(s);
    }

    pub fn clear(&mut self) {
        self.data.clear();
        self.sum_mbps = 0.0;
        self.sum_disk = 0.0;
    }

    pub fn len(&self) -> usize { self.data.len() }
    pub fn is_empty(&self) -> bool { self.data.is_empty() }

    pub fn points(&self) -> egui_plot::PlotPoints {
        egui_plot::PlotPoints::from(
            self.data.iter().map(|s| [s.time, s.mbps]).collect::<Vec<_>>()
        )
    }

    pub fn disk_points(&self) -> egui_plot::PlotPoints {
        egui_plot::PlotPoints::from(
            self.data.iter().map(|s| [s.time, s.disk_mbps]).collect::<Vec<_>>()
        )
    }

    pub fn ma(&self, window: usize) -> egui_plot::PlotPoints {
        let mut pts = Vec::with_capacity(self.data.len());
        let v: Vec<&Sample> = self.data.iter().collect();
        for i in 0..v.len() {
            if i + 1 < window { continue; }
            let sum: f64 = v[i + 1 - window..=i].iter().map(|s| s.mbps).sum();
            pts.push([v[i].time, sum / window as f64]);
        }
        egui_plot::PlotPoints::from(pts)
    }

    pub fn last(&self) -> Option<&Sample> { self.data.back() }
    pub fn samples(&self) -> impl Iterator<Item = &Sample> { self.data.iter() }

    pub fn last_n_avg(&self, n: usize) -> f64 {
        if self.data.len() < n || n == 0 { return 0.0; }
        let start = self.data.len() - n;
        let sum: f64 = self.data.iter().skip(start).map(|s| s.mbps).sum();
        sum / n as f64
    }

    pub fn stats(&self) -> (f64, f64, f64) {
        if self.data.is_empty() { return (0.0, 0.0, 0.0); }
        let avg = self.sum_mbps / self.data.len() as f64;
        let max = self.data.iter().map(|s| s.mbps).fold(f64::MIN, f64::max);
        let min = self.data.iter().map(|s| s.mbps).fold(f64::MAX, f64::min);
        (avg, max, min)
    }
}
