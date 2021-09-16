use std::sync::{Arc};
use tokio::sync::{Mutex, RwLock, RwLockWriteGuard};

use songbird::{
    {Songbird, Call},
    {ytdl, tracks::create_player},
    tracks::{TrackHandle},
    driver::Bitrate,
    Event,
    EventContext,
    EventHandler as SongBirdEventHandler,
    TrackEvent,
};

use serenity::{
    prelude::*,
    async_trait,
    model::{id::{ChannelId, UserId}},
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::channel::{Message, ChannelType, GuildChannel},
};

// For our url regex matching
use regex::Regex;

#[derive(Debug, Clone)]
pub struct PlayerData {
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    track_handle: Option<TrackHandle>,
    songbird: Arc<Songbird>,
}

impl PlayerData {
    pub fn new(songbird_handle: Arc<Songbird>) -> PlayerData {
        PlayerData {
            call_handle_lock: None,
            track_handle: None,
            songbird: songbird_handle,
        }
    }
}

pub struct MusicHandler;

impl TypeMapKey for MusicHandler {
    type Value = PlayerData;
}

pub struct AudioPlayer {
    pub songbird: Arc<Songbird>,
    player_data: Arc<RwLock<PlayerData>>,
    user_id: Option<UserId>,
}

impl AudioPlayer {
    pub fn new() -> AudioPlayer {
        let songbird = Songbird::serenity();
        AudioPlayer {
            songbird: songbird.clone(),
            player_data: Arc::new(RwLock::new(PlayerData {
                call_handle_lock: None,
                track_handle: None,
                songbird: songbird.clone(),
            })),     
            user_id: None,
        }
    }

    pub fn songbird(&self) -> Arc<Songbird> {
        return self.songbird.clone()
    }

    // The reset presence and activity action for both ready and result
    async fn set_status(&self, ctx: &Context) {
        ctx.reset_presence().await;
        ctx.set_activity(Activity::watching("the sniffer")).await;
    }

    async fn process_play(&self, ctx: Context, new_message: Message) {
        lazy_static! {
            // Returns the whole string to replace in the first capture, contents of [] in 2nd and () in 3rd
            static ref RE: Regex = Regex::new(r"https://\S*youtu\S*").unwrap();
        }

        //let mut player_data = self.player_data.write().await;

        match RE.captures(new_message.content.as_str()) {
            None => {
                error!("told to play, but nothing given");
            }
            Some(r) => {
                warn!("Told to play");

                let mut player_data = self.player_data.write().await;

                let url_to_play = &r[0];
                // the bitrate we're going to use
                let mut bitrate = Bitrate::Auto;
                // Join our channel if we haven't yet
                match &player_data.call_handle_lock {
                    Some(_) => {
                        // already here
                        warn!("We're already in the call, just start playing");
                    }
                    None => {
                        // need to join
                        warn!("Not in a call, need to join");
                        // Find who summoned us
                        let summoner = new_message.author;
                        warn!("{} ({}) is summoning", summoner.name, summoner.id);

                        let current_guild_id = new_message.guild_id.expect("No guild id in this message");
                        let mut voice_channels = current_guild_id.channels(ctx.http).await.unwrap().values().cloned().collect::<Vec<GuildChannel>>();
                        // remove all non-voice channels
                        voice_channels.retain(|x| x.kind == ChannelType::Voice);

                        let mut channel_id = ChannelId::from(0);
                        // Look for our members
                        for channel in voice_channels {
                            for member in channel.members(ctx.cache.clone()).await.unwrap() {
                                if member.user == summoner {
                                    channel_id = channel.id;
                                    warn!("found our summoner \"{}\" in channel \"{}\"", member.user.name, channel.name);
                                    warn!("bitrate is {}", channel.bitrate.unwrap());
                                    bitrate = Bitrate::BitsPerSecond(channel.bitrate.unwrap() as i32);
                                }
                            }
                        }
                        if *channel_id.as_u64() != (0 as u64) {
                            let (handler_lock, conn_result) = self.songbird.join(current_guild_id, channel_id).await;
                            conn_result.expect("Error creating songbird call");
                            // Record our call handle
                            player_data.call_handle_lock = Some(handler_lock);
                        }
                        else {
                            error!("we couldn't find our guy");
                            return;
                        }   
                    }
                }
                // drop our write lock before the super long process of pulling a video
                drop(player_data);
            
                // Create our player
                let youtube_input = ytdl(url_to_play).await.expect("Error creating ytdl input");
                let metadata = youtube_input.metadata.clone();
                warn!("Loaded up youtube: {} - {}", metadata.title.unwrap(), metadata.source_url.unwrap());
                let (audio, track_handle) = create_player(youtube_input);
                // Give it the handle to end the call if need be
                track_handle.add_event(
                    Event::Track(TrackEvent::End),
                    TrackEndCallback {
                        player_data: self.player_data.clone(),
                    }
                ).expect("Failed to add handle to track");
                // Record our track object
                let mut player_data = self.player_data.write().await; // lock it again
                player_data.track_handle = Some(track_handle);
            
                // Start playing our audio
                let mut call_handle = player_data.call_handle_lock.as_ref().expect("Absolutely should have lock here").lock().await;
                call_handle.set_bitrate(bitrate);
                call_handle.play_only(audio);

                warn!("Playing a track!"); 
            }
        }
    }
}

#[async_trait]
impl EventHandler for AudioPlayer {

    async fn ready(&self, ctx: Context, ready: Ready) {
        warn!("Connected as {}, setting bot to online", ready.user.name);
        self.set_status(&ctx).await;
    }

    async fn resume(&self, ctx: Context, _: ResumedEvent) {
        warn!("Resumed (reconnected)");
        self.set_status(&ctx).await;
    }

    async fn message(&self, ctx: Context, new_message: Message) {

        if new_message.channel_id == ChannelId::from(766900346202882058) {
            // TODO: It'd be best not to get a lock on every message coming into this channel
            // but for right now we should be okay
            //let mut data = ctx.data.write().await;
            //let mut player_data = data.get_mut::<MusicHandler>().expect("Error getting call handler");
            // Get the lock on our data so we can modify it
            //let player_data_r = self.player_data.read().await;
            match new_message.content.as_str() {
                "leave" => {
                    warn!("Told to leave");
                    match { &self.player_data.read().await.call_handle_lock } {
                        Some(h) => {
                            {
                                let mut call_handler = h.lock().await;
                                call_handler.leave().await.expect("Error leaving call");
                            }
                            // Wipe everything
                            {
                                let mut player_data = self.player_data.write().await;
                                player_data.call_handle_lock = None;
                                player_data.track_handle = None;
                            }
                        }
                        None => {
                            error!("Told do leave but we weren't here");
                        }
                    }
                }
                "stop" => {
                    warn!("Told to stop");
                    match { &self.player_data.read().await.track_handle } {
                        Some(h) => {
                            h.stop().expect("Error stopping track");
                            // Wipe our track handle
                            {
                                let mut player_data = self.player_data.write().await;
                                player_data.track_handle = None;
                            }
                        }
                        None => {
                            warn!("Told to stop, but not playing anything")
                        }
                    }
                }
                "pause" => {
                    warn!("Told to pause");
                    match { &self.player_data.read().await.track_handle } {
                        Some(h) => {
                            h.pause().expect("Error pausing track");
                        }
                        None => {
                            warn!("Told to pause, but not playing anything")
                        }
                    }
                }
                "resume" => {
                    warn!("Told to resume");
                    match { &self.player_data.read().await.track_handle } {
                        Some(h) => {
                            h.play().expect("Error resuming track");
                        }
                        None => {
                            warn!("Told to resume, but not playing anything")
                        }
                    }
                }
                // Do our play matching below because "match" doesn't play well with contains
                _ => {
                    if new_message.content.contains("play") {
                        self.process_play(ctx, new_message).await;
                    }
                    else {
                        error!("We got a message here, but it isn't any we are interested in");
                    }
                }
            }
        }
    }
}

// Very specific struct only for the purpose of leaving the call if nothing is playing after an idle timeout
struct TrackEndCallback {
    player_data: Arc<RwLock<PlayerData>>,
}

#[async_trait]
impl SongBirdEventHandler for TrackEndCallback {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        /*
        {
            let player_data = self.player_data.read().await;
            error!("Event fired, got info: {:?}", player_data.track_handle.as_ref().unwrap().get_info().await);
            error!("Waiting a few seconds to get it again");
        }
        */
        warn!("Timeout callback fired");
        std::thread::sleep(std::time::Duration::from_secs(30));

        // Make sure we're still in a call
        let mut player_data = self.player_data.write().await;
        if player_data.call_handle_lock.is_none() {
            // We're not in a call anymore, get out of this routine
            return None;
        }

        // Check to see if we're playing anything
        match &player_data.track_handle {
            Some(h) => {
                warn!("We've still got a track handle... seeing if its playing");
                match h.get_info().await {
                    Ok(_) => {
                        warn!("Still playing, leave it be");
                    }
                    Err(_) => {
                        warn!("We're sitting idle, hang up");
                        {
                            let mut call_lock = player_data.call_handle_lock.as_ref().expect("call lock none when it shouldn't be")
                            .lock().await;
                            call_lock.leave().await.expect("Error leaving call");
                        }
                        player_data.call_handle_lock = None;
                        warn!("Reset call handle");
                    }
                }
            }
            None => {
                warn!("No track handle");
            }
        }
        
        None
    }
}