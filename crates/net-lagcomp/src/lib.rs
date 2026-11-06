//! Lag compensation: a ring of hitbox positions per tick so the server can resolve a shot
//! against the world the shooter saw (`tick - rtt/2 - interpolation`).

use net_common::types::Tick;
use net_sim::EntityId;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hitbox {
    pub center: [f32; 3],
    /// Axis-aligned half extents.
    pub half: [f32; 3],
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    pub entity: EntityId,
    pub distance: f32,
    pub point: [f32; 3],
}

#[derive(Debug)]
pub struct History {
    window: usize,
    frames: Vec<(u32, Vec<(EntityId, Hitbox)>)>,
    head: usize,
    len: usize,
    index: HashMap<u32, usize>,
}

impl History {
    /// `window` ticks of history (at 60 Hz, 60 covers one second of latency).
    pub fn new(window: usize) -> Self {
        let window = window.max(1);
        Self {
            window,
            frames: Vec::with_capacity(window),
            head: 0,
            len: 0,
            index: HashMap::new(),
        }
    }

    pub fn record(&mut self, tick: Tick, boxes: Vec<(EntityId, Hitbox)>) {
        if self.frames.len() < self.window {
            self.frames.push((tick.0, boxes));
            self.index.insert(tick.0, self.frames.len() - 1);
            self.len = self.frames.len();
            self.head = self.len % self.window;
            return;
        }
        let evicted = self.frames[self.head].0;
        self.index.remove(&evicted);
        self.frames[self.head] = (tick.0, boxes);
        self.index.insert(tick.0, self.head);
        self.head = (self.head + 1) % self.window;
    }

    pub fn oldest(&self) -> Option<Tick> {
        self.index.keys().min().map(|t| Tick(*t))
    }

    pub fn at(&self, tick: Tick) -> Option<&[(EntityId, Hitbox)]> {
        self.index
            .get(&tick.0)
            .map(|i| self.frames[*i].1.as_slice())
    }

    /// Hitboxes at `tick`; when that tick is older than the window the oldest frame is used,
    /// when it is in the future the newest one. Returns the tick actually used.
    pub fn at_or_nearest(&self, tick: Tick) -> Option<(Tick, &[(EntityId, Hitbox)])> {
        if let Some(b) = self.at(tick) {
            return Some((tick, b));
        }
        let (mn, mx) = (self.index.keys().min()?, self.index.keys().max()?);
        let t = if tick.0 < *mn { *mn } else { *mx };
        self.at(Tick(t)).map(|b| (Tick(t), b))
    }

    /// Nearest hit along a ray at `tick`, skipping `ignore` (the shooter).
    pub fn raycast_at(
        &self,
        tick: Tick,
        origin: [f32; 3],
        dir: [f32; 3],
        max_dist: f32,
        ignore: Option<EntityId>,
    ) -> Option<Hit> {
        let (_, boxes) = self.at_or_nearest(tick)?;
        let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        if len <= 0.0 {
            return None;
        }
        let d = [dir[0] / len, dir[1] / len, dir[2] / len];
        let mut best: Option<Hit> = None;
        for (id, hb) in boxes {
            if Some(*id) == ignore {
                continue;
            }
            if let Some(t) = ray_aabb(origin, d, hb) {
                if t <= max_dist && best.is_none_or(|b| t < b.distance) {
                    best = Some(Hit {
                        entity: *id,
                        distance: t,
                        point: [
                            origin[0] + d[0] * t,
                            origin[1] + d[1] * t,
                            origin[2] + d[2] * t,
                        ],
                    });
                }
            }
        }
        best
    }

    /// Entities whose hitbox intersects a sphere at `tick` (explosions, melee).
    pub fn overlap_sphere_at(&self, tick: Tick, center: [f32; 3], radius: f32) -> Vec<EntityId> {
        let Some((_, boxes)) = self.at_or_nearest(tick) else {
            return Vec::new();
        };
        boxes
            .iter()
            .filter(|(_, hb)| {
                let d2: f32 = hb
                    .center
                    .iter()
                    .zip(&hb.half)
                    .zip(&center)
                    .map(|((c, h), p)| {
                        let q = p.clamp(c - h, c + h);
                        (p - q) * (p - q)
                    })
                    .sum();
                d2 <= radius * radius
            })
            .map(|(id, _)| *id)
            .collect()
    }
}

/// Slab test; returns entry distance (0 when origin is inside).
fn ray_aabb(o: [f32; 3], d: [f32; 3], hb: &Hitbox) -> Option<f32> {
    let mut tmin = 0.0f32;
    let mut tmax = f32::INFINITY;
    for i in 0..3 {
        let lo = hb.center[i] - hb.half[i];
        let hi = hb.center[i] + hb.half[i];
        if d[i].abs() < 1e-8 {
            if o[i] < lo || o[i] > hi {
                return None;
            }
            continue;
        }
        let inv = 1.0 / d[i];
        let (mut t1, mut t2) = ((lo - o[i]) * inv, (hi - o[i]) * inv);
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        tmin = tmin.max(t1);
        tmax = tmax.min(t2);
        if tmin > tmax {
            return None;
        }
    }
    Some(tmin)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(i: u32) -> EntityId {
        EntityId {
            index: i,
            generation: 0,
        }
    }
    fn hb(x: f32) -> Hitbox {
        Hitbox {
            center: [x, 0.0, 0.0],
            half: [0.5, 1.0, 0.5],
        }
    }

    #[test]
    fn ring_keeps_window_and_rewinds() {
        let mut h = History::new(4);
        for t in 0..10u32 {
            h.record(Tick(t), vec![(e(1), hb(t as f32))]);
        }
        assert_eq!(h.oldest(), Some(Tick(6)));
        assert!(h.at(Tick(5)).is_none());
        assert_eq!(h.at(Tick(7)).unwrap()[0].1.center[0], 7.0);
        // too old -> oldest frame, future -> newest
        assert_eq!(h.at_or_nearest(Tick(2)).unwrap().0, Tick(6));
        assert_eq!(h.at_or_nearest(Tick(99)).unwrap().0, Tick(9));
    }

    #[test]
    fn raycast_hits_where_target_was() {
        let mut h = History::new(8);
        h.record(Tick(10), vec![(e(1), hb(10.0)), (e(2), hb(20.0))]);
        h.record(Tick(11), vec![(e(1), hb(50.0)), (e(2), hb(20.0))]);
        // shooter saw tick 10: entity 1 at x=10, nearest along +x
        let hit = h
            .raycast_at(Tick(10), [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 100.0, None)
            .unwrap();
        assert_eq!(hit.entity, e(1));
        assert!((hit.distance - 9.5).abs() < 1e-4);
        // at tick 11 entity 1 moved away; entity 2 is nearest
        let hit = h
            .raycast_at(Tick(11), [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 100.0, None)
            .unwrap();
        assert_eq!(hit.entity, e(2));
        // ignore shooter, range limit, miss
        assert_eq!(
            h.raycast_at(
                Tick(10),
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                100.0,
                Some(e(1))
            )
            .unwrap()
            .entity,
            e(2)
        );
        assert!(h
            .raycast_at(Tick(10), [0.0, 0.0, 0.0], [1.0, 0.0, 0.0], 5.0, None)
            .is_none());
        assert!(h
            .raycast_at(Tick(10), [0.0, 5.0, 0.0], [1.0, 0.0, 0.0], 100.0, None)
            .is_none());
        assert!(h
            .raycast_at(Tick(10), [0.0, 0.0, 0.0], [0.0, 0.0, 0.0], 100.0, None)
            .is_none());
    }

    #[test]
    fn sphere_overlap() {
        let mut h = History::new(2);
        h.record(
            Tick(1),
            vec![(e(1), hb(0.0)), (e(2), hb(3.0)), (e(3), hb(9.0))],
        );
        let mut ids = h.overlap_sphere_at(Tick(1), [1.0, 0.0, 0.0], 2.0);
        ids.sort();
        assert_eq!(ids, vec![e(1), e(2)]);
        assert!(h
            .overlap_sphere_at(Tick(1), [100.0, 0.0, 0.0], 1.0)
            .is_empty());
    }
}
