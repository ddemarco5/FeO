use std::sync::{Arc};
use tokio::sync::{Mutex, RwLock};

use songbird::{
    {Songbird, Call},
    {ytdl, tracks::create_player},
    tracks::{Track, TrackHandle, PlayMode},
    driver::Bitrate,
    Event,
    EventContext,
    EventHandler as SongBirdEventHandler,
    TrackEvent,
    input::error::Error,
};

use serenity::{
    prelude::*,
    async_trait,
    model::{id::{ChannelId}},
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::prelude::User,
    model::channel::{Message, ChannelType, GuildChannel},
};

// For our url regex matching
use regex::Regex;

/*
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
*/
/*
pub struct MusicHandler;

impl TypeMapKey for MusicHandler {
    type Value = PlayerData;
}
*/
#[derive(Clone)]
pub struct AudioPlayer {
    //pub songbird: Arc<Songbird>,
    //player_data: Arc<RwLock<PlayerData>>,
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    track_handle: Option<TrackHandle>,
    songbird: Arc<Songbird>,
}


impl AudioPlayer {
    pub fn new() -> AudioPlayer {
        AudioPlayer {
        call_handle_lock: None,
        track_handle: None,
        songbird: Songbird::serenity(),
        }
        
            /*
            player_data: Arc::new(RwLock::new(PlayerData {
                call_handle_lock: None,
                track_handle: None,
                songbird: songbird.clone(),
            })),
            */
            //user_id: None,
    }

    /// Returns the event handler to give to serenity
    pub fn get_handler(&self) -> AudioPlayerHandler {
        AudioPlayerHandler{
            audio_player: Arc::new(Mutex::new(self.clone())),
        }
    }

    pub fn set_call_handle(&mut self, new_handle: Arc<Mutex<Call>>) {
        /*
        let mut data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.call_handle_lock.write().await
            })
        });
        data.call_handle_lock = Some(new_handle);
        */
        self.call_handle_lock = Some(new_handle);
    }

    pub fn clear_call_handle(&mut self) {
        /*
        let mut data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.player_data.write().await
            })
        });
        */
        self.call_handle_lock = None;
    }

    pub fn set_track_handle(&mut self, new_handle: TrackHandle) {
        /*
        let mut data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.player_data.write().await
            })
        });
        */
        self.track_handle = Some(new_handle);
    }

    pub fn clear_track_handle(&mut self) {
        /*
        let mut data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.player_data.write().await
            })
        });
        */
        self.track_handle = None;
    }

    pub fn get_call_handle(&self) -> Option<Arc<Mutex<Call>>> {
        /*
        let data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.player_data.read().await
            })
        });
        match &data.call_handle_lock {
            Some(c) => {
                return Some(c.clone());
            }
            None => {
                return None;
            }
        }
        */
        return self.call_handle_lock.clone();
    }

    pub fn get_track_handle(&self) -> Option<TrackHandle> {
        /*
        let data = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
            self.player_data.read().await
            })
        });
        match &data.track_handle {
            Some(t) => {
                return Some(t.clone());
            }
            None => {
                return None;
            }
        }
        */
        return self.track_handle.clone();
    }

    pub fn get_songbird(&self) -> Arc<Songbird> {
        return self.songbird.clone()
    }

    // The reset presence and activity action for both ready and result
    async fn set_status(&self, ctx: &Context) {
        ctx.reset_presence().await;
        ctx.set_activity(Activity::watching("the sniffer")).await;
    }

    pub fn pause(&self) {
        match &self.track_handle {
            Some(t) => {
                let info = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        t.get_info().await
                    })
                });
                match info {
                    Ok(info) => {
                        match info.playing {
                            PlayMode::Play => {
                                warn!("Pausing track");
                                t.pause().expect("Error pausing track");
                            }
                            _ => {
                                warn!("No track playing");
                            }
                        }
                    }
                    Err(e) => {
                        error!("Couldn't get track info: {}", e);
                    }
                }
            }
            None => {
                warn!("No Track");
            }
        }  
    }

    pub fn resume(&self) {
        match &self.track_handle {
            Some(t) => {
                let info = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        t.get_info().await
                    })
                });
                match info {
                    Ok(info) => {
                        match info.playing {
                            PlayMode::Pause => {
                                warn!("Resuming track");
                                t.play().expect("Error playing track");
                            }
                            _ => {
                                warn!("No track paused");
                            }
                        }
                    }
                    Err(e) => {
                        error!("Couldn't get track info: {}", e);
                    }
                }
            }
            None => {
                warn!("No Track");
            }
        }  
    }

    pub fn stop(&mut self) {
        match self.get_track_handle() {
            Some(h) => {
                h.stop().expect("Error stopping track");
                self.clear_track_handle();
            }
            None => {
                warn!("Told to stop, but not playing anything")
            }
        }
        warn!("Stopped track");
    }

    pub fn hangup(&mut self) {
        match self.get_call_handle() {
            Some(h) => {
                tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        let mut call_handler = h.lock().await;
                        call_handler.leave().await.expect("Error leaving call");
                    })
                });
                self.stop();
                self.clear_track_handle();
                self.clear_call_handle();
            }
            None => {
                error!("Told do leave but we weren't here");
            }
        }
    }

    fn set_bitrate(&self, bitrate: Bitrate) {

    }

    async fn join_summoner(&mut self, new_message: &Message, ctx: &Context) -> Result<(), ()> {

        let summoner = new_message.author.clone();
        warn!("{} ({}) is summoning", summoner.name, summoner.id);
        let current_guild_id = new_message.guild_id.expect("No guild id in this message");
        let mut voice_channels = current_guild_id.channels(&ctx.http).await.unwrap().values().cloned().collect::<Vec<GuildChannel>>();
        // remove all non-voice channels
        voice_channels.retain(|x| x.kind == ChannelType::Voice);
        // Look for our members
        for channel in voice_channels {
            for member in channel.members(ctx.cache.clone()).await.unwrap() {
                match member.user {
                    // We found them!
                    summoner => {
                        warn!("found our summoner \"{}\" in channel \"{}\"", summoner.name, channel.name);
                        warn!("bitrate is {}", channel.bitrate.unwrap());
                        let bitrate = Bitrate::BitsPerSecond(channel.bitrate.unwrap() as i32);
                        // Join the call
                        let (handler_lock, conn_result) = self.songbird.join(current_guild_id, channel.id).await;
                        conn_result.expect("Error creating songbird call");

                        // Set our call's bitrate
                        {
                            let mut call = handler_lock.lock().await;
                            call.set_bitrate(bitrate);
                        }
                        self.set_call_handle(handler_lock);
                        return Ok(());
                    }
                    // Couldn't find them :(
                    _ => {
                        warn!("we couldn't find our guy");
                        return Err(());
                    }
                    
                }
            }
        }
        // If we get here for some reason, return nothing
        return Err(());
    }

    async fn make_yt_track(&mut self, url: &str) -> Result<Track, Error> {
        // Create our player
        let youtube_input = ytdl(url).await?;
        let metadata = youtube_input.metadata.clone();
        warn!("Loaded up youtube: {} - {}", metadata.title.unwrap(), metadata.source_url.unwrap());
        let (audio, track_handle) = create_player(youtube_input);
        // Give it the handle to end the call if need be
        track_handle.add_event(
            Event::Track(TrackEvent::End),
            TrackEndCallback {
                //player_data: self.player_data.clone(),
                audio_player: self.clone(),
            }
        ).expect("Failed to add handle to track");
        // Record our track object
        self.set_track_handle(track_handle);
        return Ok(audio);
        
    }

    fn play_only_track(&self, track: Track) {
        // Start playing our audio
        match &self.call_handle_lock {
            Some(c) => {
                tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        let mut call = c.lock().await;
                        call.play_only(track);
                    })
                });
            }
            None => {
                warn!("Can't play, not in a call");
            }
        }
        
    }

    async fn process_play(&mut self, ctx: Context, new_message: Message) {
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
                let url_to_play = &r[0];
                warn!("Told to play {}", url_to_play);
                // Join our channel if we haven't yet
                if self.get_call_handle().is_none() {
                    // need to join
                    warn!("Not in a call, need to join");
                    // Join who summoned us
                    match self.join_summoner(&new_message, &ctx).await {
                        Ok(_) => {
                            warn!("Joined summoner");
                            let track = self.make_yt_track(url_to_play).await;
                            match track {
                                Ok(t) => {
                                    warn!("Successfully created track, playing");
                                    // play our track
                                    self.play_only_track(t);
                                }
                                Err(e) => {
                                    error!("Couldn't create youtube track: {}", e);
                                    // Leave bc we can't play shit
                                    self.hangup();
                                }
                            }
                        }
                        Err(_) => warn!("Couldn't find our summoner"),
                    }
                }
            }
        }
    }
}

pub struct AudioPlayerHandler {
    audio_player: Arc<Mutex<AudioPlayer>>,
}

#[async_trait]
impl EventHandler for AudioPlayerHandler {

    async fn ready(&self, ctx: Context, ready: Ready) {
        warn!("Connected as {}, setting bot to online", ready.user.name);
        let player = self.audio_player.lock().await;
        player.set_status(&ctx).await;
    }

    async fn resume(&self, ctx: Context, _: ResumedEvent) {
        warn!("Resumed (reconnected)");
        let player = self.audio_player.lock().await;
        player.set_status(&ctx).await;
    }

    async fn message(&self, ctx: Context, new_message: Message) {

        if new_message.channel_id == ChannelId::from(766900346202882058) {
            // TODO: It'd be best not to get a lock on every message coming into this channel
            // but for right now we should be okay
            //let mut data = ctx.data.write().await;
            //let mut player_data = data.get_mut::<MusicHandler>().expect("Error getting call handler");
            // Get the lock on our data so we can modify it
            //let player_data = self.player_data.write().await;
            match new_message.content.as_str() {
                "leave" => {
                    warn!("Told to leave");
                    let mut player = self.audio_player.lock().await;
                    player.hangup();
                }
                "stop" => {
                    warn!("Told to stop");
                    let mut player = self.audio_player.lock().await;
                    player.stop();
                }
                "pause" => {
                    warn!("Told to pause");
                    let player = self.audio_player.lock().await;
                    player.pause();
                }
                "resume" => {
                    warn!("Told to resume");
                    let player = self.audio_player.lock().await;
                    player.resume();
                }
                // Do our play matching below because "match" doesn't play well with contains
                _ => {
                    if new_message.content.contains("play") {
                        let mut player = self.audio_player.lock().await;
                        player.process_play(ctx, new_message).await;
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
    //player_data: Arc<RwLock<PlayerData>>,
    audio_player: AudioPlayer,
}

#[async_trait]
impl SongBirdEventHandler for TrackEndCallback {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        /*
        {
            let player_data = self.player_data.read().await;
            error!("Event fired, got info: {:?}", player_data.track_handle.as_ref().unwrap().get_info().await);
            error!("Waiting a few seconds to get it again");
        }
        */
        warn!("Track end callback fired (timeout routine)");
        std::thread::sleep(std::time::Duration::from_secs(30));

        //match self.audio_player.call_handle_lock

        /*
        // Check if we're in a call and abort before we get locks or anything
        if self.player_data.as_ref().read().await.call_handle_lock.is_none() {
            // We're not in a call anymore, get out of this routine
            warn!("not in a call, aborting");
            return None;
        }

        // Make sure we're still in a call
        let mut player_data = self.player_data.try_write().expect("Failed to get the write in callback!");
        error!("Got lock");

        // Check to see if we're playing anything
        match &player_data.track_handle {
            // We have a track handle
            Some(h) => {
                warn!("We've still got a track handle... seeing if its playing");
                // See if our track handle is active
                match h.get_info().await {
                    Ok(_) => {
                        warn!("Still playing, leave it be");
                    }
                    // If not, hang up
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
            // No track handle (nothing playing), if we're in a call, leave it
            None => {
                warn!("No track handle, attempting to leave call");
                match &player_data.call_handle_lock {
                    Some(c) => {
                        warn!("We've got a call, leaving it");
                        {
                            let mut call_lock = c.lock().await;
                            call_lock.leave().await.expect("Error leaving call");
                        }
                        player_data.call_handle_lock = None;
                        warn!("Reset call handle");
                    }
                    None => {
                        error!("Not in a call (but this should never hit)");
                    }
                }
            }
        }
        */
        return None;
    }
}