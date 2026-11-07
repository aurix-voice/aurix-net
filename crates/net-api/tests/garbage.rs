//! Sync and lockstep rooms fed with random and corrupted data frames keep ticking.
use bytes::Bytes;
use net_api::lockstep::LockstepRoom;
use net_api::sync::SyncRoom;
use net_common::ActorId;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() % (max as u64 + 1)) as usize;
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn sync_room_survives_garbage() {
    let mut rng = Rng(3);
    let mut room = SyncRoom::new(None, 60, 1).unwrap();
    let actors = [ActorId(1), ActorId(2), ActorId(3)];
    for a in actors {
        room.on_join(a);
    }
    for i in 0..6_000u32 {
        let a = actors[(rng.next() % 3) as usize];
        let mut b = rng.bytes(200);
        if !b.is_empty() && rng.next().is_multiple_of(2) {
            b[0] = [0x10, 0x11, 0x12, 0x13][(rng.next() % 4) as usize];
        }
        let _ = room.handle(a, Bytes::from(b));
        if i.is_multiple_of(7) {
            let _ = room.tick();
        }
    }
    let _ = room.checkpoint();
}

#[test]
fn lockstep_room_survives_garbage() {
    let mut rng = Rng(4);
    let mut room = LockstepRoom::new(60, 1);
    let actors = [ActorId(1), ActorId(2)];
    for a in actors {
        room.on_join(a);
    }
    for i in 0..6_000u32 {
        let a = actors[(rng.next() % 2) as usize];
        let mut b = rng.bytes(300);
        if !b.is_empty() && rng.next().is_multiple_of(2) {
            b[0] = [0x10, 0x20, 0x21, 0x22, 0x23, 0x24][(rng.next() % 6) as usize];
        }
        let _ = room.handle(a, Bytes::from(b.clone()));
        let _ = LockstepRoom::decode_bundles(Bytes::from(b.clone()));
        let _ = LockstepRoom::decode_catchup(Bytes::from(b));
        if i.is_multiple_of(5) {
            let dg = room.tick();
            let _ = LockstepRoom::decode_bundles(dg);
        }
        if i.is_multiple_of(300) {
            let _ = room.catchup_for(false);
            let snap = room.snapshot();
            room = LockstepRoom::restore(snap);
        }
    }
}
