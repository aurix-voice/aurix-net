//! A client that runs the room's module over the confirmed inputs and reports its state hash
//! the way `docs/sdk.md` tells it to agrees with the node, which runs the same module. Like
//! the SDKs, it keeps the commands of a tick out of the players' inputs and hands them to the
//! game one by one before the tick. One that runs ahead of the confirmed ticks does the same in
//! its confirmed state.

mod common;

use bytes::{Buf, Bytes};
use common::{Node, Project};
use net_api::sync::ARENA_SCHEMA;
use net_client::lockstep::{
    take_commands, Command, LockstepClient, LsEvent, Outgoing, TickInputs, COMMAND_ACTOR,
    LS_COMMAND,
};
use net_client::rollback::{Rollback, Simulation};
use net_client::{Client, Event, Options};
use net_common::types::ActorId;
use net_common::Channel;
use net_game::{StepInput, WasmLogic};
use net_rooms::protocol::{ClientMsg, ServerMsg};
use net_sim::{EntityId, Frame, FrameConfig};
use net_wire::Schema;
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const ARENA: &str = include_str!("../../net-game/examples/arena.wat");
const TICK_HZ: u16 = 60;

fn send(client: &Client, frame: Outgoing) {
    let channel = if frame.reliable {
        Channel::ReliableOrdered
    } else {
        Channel::UnreliableSequenced
    };
    client.send_data(channel, frame.payload).expect("sends");
}

/// The game of the arena room: the room's module over a frame.
struct Game {
    frame: Frame,
    logic: WasmLogic,
    avatars: BTreeMap<ActorId, Option<EntityId>>,
    /// The ticks stepped with commands, once for every such step.
    commanded: Vec<u32>,
}

impl Simulation for Game {
    type Snapshot = (Vec<u8>, BTreeMap<ActorId, Option<EntityId>>);

    fn step(&mut self, tick: u32, inputs: &TickInputs, commands: &[Command]) {
        if !commands.is_empty() {
            self.commanded.push(tick);
        }
        for actor in inputs.keys().map(|actor| ActorId(*actor)) {
            if !self.avatars.contains_key(&actor) {
                let avatar = self.logic.on_join(&mut self.frame, actor);
                self.avatars.insert(actor, avatar);
            }
        }
        let step: Vec<StepInput> = self
            .avatars
            .iter()
            .map(|(actor, avatar)| {
                let input = inputs.get(&actor.0);
                (
                    *actor,
                    *avatar,
                    input.map(|input| input.to_vec()).unwrap_or_default(),
                    input.is_some(),
                )
            })
            .collect();
        let logic = &mut self.logic;
        self.frame.advance(|frame| {
            for command in commands {
                logic.on_command(frame, ActorId(command.actor), &command.data);
            }
            logic.step(frame, &step);
        });
    }

    fn snapshot(&self) -> Self::Snapshot {
        (self.frame.snapshot(), self.avatars.clone())
    }

    fn restore(&mut self, (frame, avatars): &Self::Snapshot) {
        self.frame.restore(frame).expect("restores");
        self.avatars = avatars.clone();
    }
}

/// A player of the arena room who runs the room's module over the ticks it receives.
struct Player {
    client: Client,
    actor: u16,
    bundles: LockstepClient,
    game: Game,
    /// Set for a player who runs ahead of the confirmed ticks.
    rollback: Option<Rollback<Game>>,
    /// Reported hashes, as (tick, hash).
    reported: Vec<(u32, u64)>,
    /// The input of the command actor in every tick that had one.
    commands: BTreeMap<u32, Vec<u8>>,
    /// The refusals the node sent, as (code, message).
    refusals: Vec<(String, String)>,
}

impl Player {
    /// From here on the player runs ahead of the confirmed ticks, by 8 at most.
    fn run_ahead(&mut self) {
        let tick = self.bundles.confirmed_tick();
        self.rollback = Some(Rollback::new(&self.game, self.actor, tick, 8));
    }

    /// Steps the confirmed state over the ticks received and reports its hash after every
    /// tenth one under that tick's number. Returns the tick numbers of the desyncs the node
    /// announced.
    fn play(&mut self) -> Vec<u32> {
        let mut desyncs = Vec::new();
        while let Some(event) = self.client.try_next_event() {
            let payload = match event {
                Event::Control(ServerMsg::Desync { tick, .. }) => {
                    desyncs.push(tick);
                    continue;
                }
                Event::Control(ServerMsg::Error { code, message, .. }) => {
                    self.refusals.push((code, message));
                    continue;
                }
                Event::Data { payload, .. } => payload,
                _ => continue,
            };
            for event in self.bundles.receive(&payload).unwrap_or_default() {
                match event {
                    LsEvent::Tick { tick, mut inputs } => {
                        if let Some(entry) = inputs.get(&COMMAND_ACTOR) {
                            self.commands.insert(tick, entry.to_vec());
                        }
                        let state = match &mut self.rollback {
                            Some(rollback) => {
                                rollback.confirm(&mut self.game, tick, &inputs);
                                rollback.confirmed_snapshot().0.clone()
                            }
                            None => {
                                let commands = take_commands(&mut inputs);
                                self.game.step(tick, &inputs, &commands);
                                self.game.frame.snapshot()
                            }
                        };
                        if tick % 10 == 0 {
                            let hash = net_rollback::hash_bytes(&state);
                            send(&self.client, LockstepClient::hash_frame(tick, hash));
                            self.reported.push((tick, hash));
                        }
                    }
                    LsEvent::Restore { state, .. } if !state.is_empty() => {
                        self.game.frame.restore(&state).expect("restores");
                    }
                    _ => {}
                }
            }
            send(&self.client, self.bundles.ack_frame());
        }
        desyncs
    }

    /// Plays for `time`, sending an input every tick, and expects no desync.
    async fn play_for(&mut self, time: Duration) {
        let until = Instant::now() + time;
        while Instant::now() < until {
            let input = Bytes::from_static(&[127, 0, 0]);
            if let Some((tick, frame)) = self.bundles.send_input(input.clone()) {
                send(&self.client, frame);
                if let Some(rollback) = &mut self.rollback {
                    rollback.local_input(tick, input);
                    rollback.predict(&mut self.game, tick + 1);
                }
            }
            self.client.flush();
            tokio::time::sleep(Duration::from_millis(1000 / u64::from(TICK_HZ))).await;
            let desyncs = self.play();
            assert!(desyncs.is_empty(), "desync at ticks {desyncs:?}");
        }
    }
}

/// Creates the arena room on `node`. Returns its creator and the id of the room.
async fn arena(node: &Node, project: &Project) -> (Player, String) {
    let upload = reqwest::Client::new()
        .put(format!("{}/v1/modules/arena", node.http))
        .bearer_auth(&project.api.token)
        .body(ARENA)
        .send()
        .await
        .expect("uploads the module");
    assert!(upload.status().is_success(), "{}", upload.status());

    let opts = Options {
        tunnel: true,
        ..Default::default()
    };
    let mut client = Client::connect(&node.http, &project.grant("ann").await, opts)
        .await
        .expect("connects");
    let create: ClientMsg = serde_json::from_value(json!({
        "t": "create", "name": "arena", "mode": "lockstep",
        "props": {
            "tick_hz": TICK_HZ,
            "logic": "arena",
            "schema": ARENA_SCHEMA,
            "position": "Transform.0",
        },
    }))
    .expect("client message");
    let joined = client.join(create).await.expect("creates");
    let seed = joined.seed.expect("seed");

    let engine = net_game::GameEngine::new();
    let (_, module) = engine.load(ARENA.as_bytes()).expect("module");
    let logic = engine.instantiate(&module, seed).expect("logic");
    let config = FrameConfig {
        tick_hz: TICK_HZ,
        ..Default::default()
    };
    let schema = Schema::parse(ARENA_SCHEMA).expect("schema");
    let player = Player {
        client,
        actor: joined.actor.0,
        bundles: LockstepClient::new(),
        game: Game {
            frame: Frame::new(schema, config, seed),
            logic,
            avatars: BTreeMap::new(),
            commanded: Vec::new(),
        },
        rollback: None,
        reported: Vec::new(),
        commands: BTreeMap::new(),
        refusals: Vec::new(),
    };
    (player, joined.room)
}

#[tokio::test]
async fn client_that_reports_hashes_as_documented_agrees_with_the_referee() {
    let node = Node::start().await;
    let project = node.project("referee").await;
    let (mut ann, _) = arena(&node, &project).await;

    ann.play_for(Duration::from_secs(3)).await;
    let reported = &ann.reported;
    assert!(reported.len() >= 10, "{reported:?}");
    let states: BTreeSet<u64> = reported.iter().map(|(_, hash)| *hash).collect();
    assert!(states.len() >= 10, "the avatar did not move: {reported:?}");

    // The node does compare: a wrong hash under the last reported tick is a desync.
    let (tick, hash) = *reported.last().expect("a hash");
    send(&ann.client, LockstepClient::hash_frame(tick, hash ^ 1));
    ann.client.flush();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(ann.play(), [tick]);
}

/// The commands in the input of the command actor of one tick, as (actor, command).
fn commands_of(mut entry: Bytes) -> Vec<(u16, Bytes)> {
    let mut commands = Vec::new();
    while entry.has_remaining() {
        let actor = entry.get_u16();
        let len = usize::from(entry.get_u16());
        commands.push((actor, entry.split_to(len)));
    }
    commands
}

#[tokio::test]
async fn commands_reach_clients_the_referee_and_the_replay_in_the_same_ticks() {
    let node = Node::start().await;
    let project = node.project("referee").await;
    let (mut ann, room) = arena(&node, &project).await;
    ann.play_for(Duration::from_millis(500)).await;

    // more than one tick carries: 255 bytes with 4 for the header of each command
    let sent: Vec<Bytes> = [100, 100, 100, 200, 200, 251, 60, 60, 60, 60]
        .iter()
        .zip(1u8..)
        .map(|(len, fill)| Bytes::from(vec![fill; *len]))
        .collect();
    for command in &sent {
        send(&ann.client, LockstepClient::command_frame(command).unwrap());
    }
    let mut too_long = vec![LS_COMMAND];
    too_long.extend_from_slice(&[9; 252]);
    ann.client
        .send_data(Channel::ReliableOrdered, too_long.into())
        .expect("sends");
    ann.play_for(Duration::from_secs(2)).await;

    assert_eq!(
        ann.refusals,
        [(
            "invalid_request".to_string(),
            "invalid request: a lockstep command takes 1 to 251 bytes, got 252".to_string()
        )]
    );
    let got: Vec<Bytes> = ann
        .commands
        .values()
        .flat_map(|entry| commands_of(Bytes::from(entry.clone())))
        .map(|(_, command)| command)
        .collect();
    assert_eq!(got, sent);
    assert!(ann.commands.len() >= 6, "{:?}", ann.commands.keys());
    assert!(ann.reported.len() >= 10, "{:?}", ann.reported);

    let replay: serde_json::Value = reqwest::Client::new()
        .get(format!("{}/v1/rooms/{room}/replay", node.http))
        .bearer_auth(&project.api.token)
        .send()
        .await
        .expect("asks for the replay")
        .json()
        .await
        .expect("replay");
    let recorded: BTreeMap<u32, Vec<u8>> = replay["inputs"]
        .as_array()
        .expect("inputs")
        .iter()
        .zip(0u32..)
        .filter_map(|(inputs, tick)| {
            let entry = inputs.get(COMMAND_ACTOR.to_string())?;
            Some((tick, serde_json::from_value(entry.clone()).expect("bytes")))
        })
        .collect();
    assert_eq!(recorded, ann.commands);
}

#[tokio::test]
async fn every_command_is_delivered_or_refused() {
    // refusals of a command are not held to the rate of the answers to unreadable frames
    let node = Node::start_with(&[("NET__LIMITS__FRAME_ERRORS_PER_SEC_PER_PLAYER", "1")]).await;
    let project = node.project("referee").await;
    let (mut ann, _) = arena(&node, &project).await;
    ann.play_for(Duration::from_millis(500)).await;

    // more than the node keeps for the ticks to come
    for fill in 1..=40 {
        let command = Bytes::from(vec![fill; 200]);
        send(
            &ann.client,
            LockstepClient::command_frame(&command).unwrap(),
        );
    }
    ann.play_for(Duration::from_secs(2)).await;

    let delivered = ann
        .commands
        .values()
        .flat_map(|entry| commands_of(Bytes::from(entry.clone())))
        .count();
    let refused = ann.refusals.len();
    assert!(
        ann.refusals.iter().all(|(code, _)| code == "rate_limited"),
        "{:?}",
        ann.refusals
    );
    assert!(refused >= 10, "{refused} refusals");
    assert_eq!(delivered + refused, 40);
}

#[tokio::test]
async fn client_that_runs_ahead_takes_a_command_in_its_confirmed_tick_only() {
    let node = Node::start().await;
    let project = node.project("referee").await;
    let (mut ann, _) = arena(&node, &project).await;
    ann.play_for(Duration::from_millis(500)).await;
    ann.run_ahead();
    ann.play_for(Duration::from_millis(500)).await;

    for step in [100, 50] {
        send(&ann.client, LockstepClient::command_frame(&[step]).unwrap());
        ann.play_for(Duration::from_millis(500)).await;
    }
    let before = ann.reported.len();
    ann.play_for(Duration::from_secs(1)).await;

    let rollback = ann.rollback.as_ref().expect("runs ahead");
    assert!(
        rollback.predicted_ticks > 60,
        "{}",
        rollback.predicted_ticks
    );
    assert!(rollback.head() > rollback.confirmed_tick());
    let commanded: Vec<u32> = ann.commands.keys().copied().collect();
    assert_eq!(commanded.len(), 2);
    assert_eq!(ann.game.commanded, commanded);
    // the node compared the hashes of the confirmed state after the commands with its own
    assert!(ann.reported.len() >= before + 5, "{:?}", ann.reported);
}
