//! # Ursa Behaviour implementation.
//!
//! Ursa custom behaviour implements [`NetworkBehaviour`] with the following options:
//!
//! - [`Ping`] A `NetworkBehaviour` that responds to inbound pings and
//!   periodically sends outbound pings on every established connection.
//! - [`Identify`] A `Networkbehaviour` that automatically identifies nodes periodically, returns information
//!   about them, and answers identify queries from other nodes.
//! - [`Bitswap`] A `Networkbehaviour` that handles sending and receiving blocks.
//! - [`Gossipsub`] A `Networkbehaviour` that handles the gossipsub protocol.
//! - [`DiscoveryBehaviour`]
//! - [`RequestResponse`] A `NetworkBehaviour` that implements a generic
//!   request/response protocol or protocol family, whereby each request is
//!   sent over a new substream on a connection.

use std::{
    collections::{HashSet, VecDeque},
    task::{Context, Poll},
};

use anyhow::Result;
use libipld::store::StoreParams;
use libp2p::{
    gossipsub::{
        error::{PublishError, SubscriptionError},
        Gossipsub, GossipsubEvent, IdentTopic as Topic,
    },
    identify::{Identify, IdentifyConfig, IdentifyEvent},
    kad::QueryId,
    ping::{Ping, PingEvent, PingFailure, PingSuccess},
    request_response::{
        ProtocolSupport, RequestResponse, RequestResponseConfig, RequestResponseEvent,
        RequestResponseMessage,
    },
    swarm::{
        NetworkBehaviour, NetworkBehaviourAction, NetworkBehaviourEventProcess, PollParameters,
    },
    NetworkBehaviour, PeerId,
};
use libp2p_bitswap::{Bitswap, BitswapConfig, BitswapEvent, BitswapStore};
use tiny_cid::Cid;
use tracing::{debug, trace};

use crate::{
    codec::proto::{
        UrsaExchangeCodec, UrsaExchangeProtocol, UrsaExchangeRequest, UrsaExchangeResponse,
    },
    config::UrsaConfig,
    discovery::behaviour::{DiscoveryBehaviour, DiscoveryEvent},
    gossipsub::UrsaGossipsub,
    service::{UrsaEvent, PROTOCOL_NAME},
    types::UrsaRequestResponseEvent,
};

/// [Behaviour]'s events
/// Requests and failure events emitted by the `NetworkBehaviour`.
#[derive(Debug)]
pub enum BehaviourEvent {
    Ping(PingEvent),
    Bitswap(BitswapEvent),
    Gossip(GossipsubEvent),
    Identify(IdentifyEvent),
    Discovery(DiscoveryEvent),
    RequestResponse(UrsaRequestResponseEvent),
}

/// A `Networkbehaviour` that handles Ursa's different protocol implementations.
///
/// The poll function must have the same signature as the NetworkBehaviour
/// function and will be called last within the generated NetworkBehaviour implementation.
///
/// The events generated [`BehaviourEvent`].
#[derive(NetworkBehaviour)]
#[behaviour(
    out_event = "BehaviourEvent",
    poll_method = "poll",
    event_process = true
)]
pub struct Behaviour<P: StoreParams> {
    /// Aliving checks.
    ping: Ping,
    // Identifying peer info to other peers.
    identify: Identify,
    /// Bitswap for exchanging data between blocks between peers.
    bitswap: Bitswap<P>,
    /// Ursa's gossiping protocol for message propagation.
    gossipsub: Gossipsub,
    /// Kademlia discovery and bootstrap.
    discovery: DiscoveryBehaviour,
    /// request/response protocol implementation for [`UrsaExchangeProtocol`]
    request_response: RequestResponse<UrsaExchangeCodec>,
    /// Ursa's emitted events.
    #[behaviour(ignore)]
    events: VecDeque<BehaviourEvent>,
}

impl<P: StoreParams> Behaviour<P> {
    pub fn new<S: BitswapStore<Params = P>>(config: &UrsaConfig, store: S) -> Self {
        let local_public_key = config.keypair.public();

        // TODO: check if UrsaConfig has configs for the behaviours, if not instaniate new ones

        // Setup the ping behaviour
        let ping = Ping::default();

        // Setup the gossip behaviour
        let gossipsub = UrsaGossipsub::new(config);

        // Setup the bitswap behaviour
        let bitswap = Bitswap::new(BitswapConfig::default(), store);

        // Setup the identify behaviour
        let identify = Identify::new(IdentifyConfig::new(PROTOCOL_NAME.into(), local_public_key));

        // Setup the discovery behaviour
        let discovery =
            DiscoveryBehaviour::new(&config).with_bootstrap_nodes(config.bootstrap_nodes.clone());

        let request_response = {
            let cfg = RequestResponseConfig::default();
            let protocols = vec![(UrsaExchangeProtocol, ProtocolSupport::Full)];

            RequestResponse::new(UrsaExchangeCodec, protocols, cfg)
        };

        Behaviour {
            ping,
            bitswap,
            identify,
            gossipsub,
            discovery,
            request_response,
            events: VecDeque::new(),
        }
    }

    pub fn peers(&mut self) -> HashSet<PeerId> {
        self.discovery.peers()
    }

    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        self.discovery.bootstrap()
    }

    pub fn subscribe(&mut self, topic: &Topic) -> Result<bool, SubscriptionError> {
        self.gossipsub.subscribe(topic)
    }

    pub fn unsubscribe(&mut self, topic: &Topic) -> Result<bool, PublishError> {
        self.gossipsub.unsubscribe(topic)
    }

    fn poll(
        &mut self,
        cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<
        NetworkBehaviourAction<
            <Self as NetworkBehaviour>::OutEvent,
            <Self as NetworkBehaviour>::ConnectionHandler,
        >,
    > {
        if !self.events.is_empty() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(self.events.remove(0)));
        }

        Poll::Pending
    }

    pub fn handle_ping(&mut self, event: PingEvent) {
        let peer = event.peer.to_base58();

        match event.result {
            Ok(result) => match result {
                PingSuccess::Pong => {
                    trace!(
                        "PingSuccess::Pong received a ping and sent back a pong to {}",
                        peer
                    );
                }
                PingSuccess::Ping { rtt } => {
                    trace!(
                        "PingSuccess::Ping with rtt {} from {} in ms",
                        rtt.as_millis(),
                        peer
                    );
                    // perhaps we can set rtt for each peer
                }
            },
            Err(err) => {
                match err {
                    PingFailure::Timeout => {
                        debug!(
                            "PingFailure::Timeout no response was received from {}",
                            peer
                        );
                        // remove peer from list of connected.
                    }
                    PingFailure::Unsupported => {
                        debug!("PingFailure::Unsupported the peer {} does not support the ping protocol", peer);
                    }
                    PingFailure::Other { error } => {
                        debug!(
                            "PingFailure::Other the ping failed with {} for reasons {}",
                            peer, error
                        );
                    }
                }
            }
        }
    }

    pub fn handle_identify(&mut self, event: IdentifyEvent) {
        match event {
            IdentifyEvent::Received { peer_id, info } => {
                trace!(
                    "Identification information {} has been received from a peer {}.",
                    info,
                    peer_id
                );

                // check if received identify is from a peer on the same network
                if info
                    .protocols
                    .iter()
                    .any(|name| name.as_bytes() == PROTOCOL_NAME)
                {
                    self.gossipsub.add_explicit_peer(&peer_id);

                    for address in info.listen_addrs {
                        self.discovery.add_address(peer_id, address);
                        self.request_response.add_address(&peer_id, address);
                    }
                }
            }
            IdentifyEvent::Sent { .. }
            | IdentifyEvent::Pushed { .. }
            | IdentifyEvent::Error { .. } => {}
        }
    }

    pub fn handle_bitswap(&mut self, event: BitswapEvent) {
        match event {
            BitswapEvent::Progress(query_id, counter) => {
                // Received a block from a peer. Includes the number of known missing blocks for a sync query.
                // When a block is received and missing blocks is not empty the counter is increased.
                // If missing blocks is empty the counter is decremented.

                // keep track of all the query ids.
            }
            BitswapEvent::Complete(query_id, result) => {
                // A get or sync query completed.
            }
        }
    }

    pub fn handle_gossipsub(&mut self, event: GossipsubEvent) {
        match event {
            GossipsubEvent::Message {
                propagation_source,
                message_id,
                message,
            } => {
                if let Ok(cid) = Cid::try_from(message.data) {
                    self.events.push_back(event.into());
                }
            }
            GossipsubEvent::Subscribed { peer_id, topic } => {
                // A remote subscribed to a topic.
                // subscribe to new topic.
            }
            GossipsubEvent::Unsubscribed { peer_id, topic } => {
                // A remote unsubscribed from a topic.
                // remove subscription.
            }
            GossipsubEvent::GossipsubNotSupported { peer_id } => {
                // A peer that does not support gossipsub has connected.
                // the scoring/rating should happen here.
                // disconnect.
            }
        }
    }

    pub fn handle_discovery(&mut self, event: DiscoveryEvent) {
        match event {
            DiscoveryEvent::Discoverd(peer_id) => todo!(),
            DiscoveryEvent::UnroutablePeer(_) => todo!(),
        }
    }

    pub fn handle_request_response(
        &mut self,
        event: RequestResponseEvent<UrsaExchangeRequest, UrsaExchangeResponse>,
    ) {
        match event {
            RequestResponseEvent::Message { peer, message } => match message {
                RequestResponseMessage::Request {
                    request_id,
                    request,
                    channel,
                } => {}
                RequestResponseMessage::Response {
                    request_id,
                    response,
                } => {}
            },
            RequestResponseEvent::OutboundFailure {
                peer,
                request_id,
                error,
            } => todo!(),
            RequestResponseEvent::InboundFailure {
                peer,
                request_id,
                error,
            } => todo!(),
            RequestResponseEvent::ResponseSent { peer, request_id } => todo!(),
        }
    }
}

impl<P: StoreParams> NetworkBehaviourEventProcess<PingEvent> for Behaviour<P> {
    fn inject_event(&mut self, event: PingEvent) {
        self.handle_ping(event)
    }
}

impl<P: StoreParams> NetworkBehaviourEventProcess<IdentifyEvent> for Behaviour<P> {
    fn inject_event(&mut self, event: IdentifyEvent) {
        self.handle_identify(event)
    }
}

impl<P: StoreParams> NetworkBehaviourEventProcess<GossipsubEvent> for Behaviour<P> {
    fn inject_event(&mut self, event: GossipsubEvent) {
        self.handle_gossipsub(event)
    }
}

impl<P: StoreParams> NetworkBehaviourEventProcess<BitswapEvent> for Behaviour<P> {
    fn inject_event(&mut self, event: BitswapEvent) {
        self.handle_bitswap(event)
    }
}

impl<P: StoreParams> NetworkBehaviourEventProcess<DiscoveryEvent> for Behaviour<P> {
    fn inject_event(&mut self, event: DiscoveryEvent) {
        self.handle_discovery(event)
    }
}

impl<P: StoreParams>
    NetworkBehaviourEventProcess<RequestResponseEvent<UrsaExchangeRequest, UrsaExchangeResponse>>
    for Behaviour<P>
{
    fn inject_event(
        &mut self,
        event: RequestResponseEvent<UrsaExchangeRequest, UrsaExchangeResponse>,
    ) {
        self.handle_request_response(event)
    }
}
