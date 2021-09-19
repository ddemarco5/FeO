use std::sync::{Arc};
use tokio::sync::{Mutex};

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
    CacheAndHttp,
    prelude::*,
    async_trait,
    model::{id::{ChannelId}},
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::channel::{Message, ChannelType, GuildChannel},
};

// For our url regex matching
use regex::Regex;

#[derive(Clone, Debug)]
pub struct AudioPlayer {
    //pub songbird: Arc<Songbird>,
    //player_data: Arc<RwLock<PlayerData>>,
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    track_handle: Option<TrackHandle>,
    songbird: Arc<Songbird>,
    idle_callback_installed: bool,
    idle_callback_struct: Option<TrackEndCallback>,
}


impl AudioPlayer {
    pub async fn new() -> (Arc<Mutex<AudioPlayer>>, AudioPlayerHandler) {
        // The actual player object
        let player = Arc::new(Mutex::new(AudioPlayer {
            call_handle_lock: None,
            track_handle: None,
            songbird: Songbird::serenity(),
            idle_callback_installed: false,
            idle_callback_struct: None,
        }));
        // The player's event handler
        let handler = AudioPlayerHandler{
            audio_player: player.clone(),
        };
        // Create the callback structure
        {
            let mut player_locked = player.lock().await;

            player_locked.idle_callback_struct = Some(TrackEndCallback {
                audio_player: player.clone(),
                timeout: std::time::Duration::from_secs(30),
            });
        }
        
        return (player, handler);
    }

    /// Give songbird the information it needs to join a call as a bots
    pub fn init_player(&self, cache_and_http: std::sync::Arc<CacheAndHttp>, shard_count: u64) {
        let bot_user_id = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                cache_and_http.http.get_current_user().await.expect("couldn't get current user").id
            })
        });
        self.songbird.initialise_client_data(shard_count, bot_user_id);
    }

    pub fn set_call_handle(&mut self, new_handle: Arc<Mutex<Call>>) {
        self.call_handle_lock = Some(new_handle);
    }

    pub fn clear_call_handle(&mut self) {
        self.call_handle_lock = None;
    }

    pub fn set_track_handle(&mut self, new_handle: TrackHandle) {
        self.track_handle = Some(new_handle);
    }

    pub fn clear_track_handle(&mut self) {
        self.track_handle = None;
    }

    pub fn get_call_handle(&self) -> Option<Arc<Mutex<Call>>> {
        return self.call_handle_lock.clone();
    }

    pub fn get_track_handle(&self) -> Option<TrackHandle> {
        return self.track_handle.clone();
    }

    pub fn get_songbird(&self) -> Arc<Songbird> {
        return self.songbird.clone()
    }

    fn add_idle_check(&mut self) {
        match &self.idle_callback_installed {
            true => {
                warn!("Idle check is already installed");
            }
            false => {
                match &self.get_call_handle() {
                    Some(h) => {
                        let mut call = tokio::task::block_in_place(move || {
                            tokio::runtime::Handle::current().block_on(async move {
                                h.lock().await
                            })
                        });
                        call.add_global_event(
                            Event::Track(TrackEvent::End),
                            // Install a copy of our callback struct as an event
                            self.idle_callback_struct.as_ref().unwrap().clone(),
                        );
                        warn!("added the idle check callback");
                        self.idle_callback_installed = true;
                    }
                    None => {
                        warn!("Can't add an event, no driver");
                    }
                }
            }
        }
    }

    /// Removes all the event handles, but we only have the idle check for now
    pub fn remove_idle_check(&mut self) {
        match &self.idle_callback_installed {
            true => {
                match &self.get_call_handle() {
                    Some(h) => {
                        let mut call = tokio::task::block_in_place(move || {
                            tokio::runtime::Handle::current().block_on(async move {
                                h.lock().await
                            })
                        });
                        call.remove_all_global_events();
                        warn!("removed the idle check callback");
                        self.idle_callback_installed = false;
                    }
                    None => {
                        warn!("No call to removed handles from");
                    }
                }
            }
            false => {
                warn!("Idle check is already removed");
            }
        }
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
                            PlayMode::End => {
                                warn!("Track is already ended");
                            }
                            _ => {
                                t.stop().expect("Error stopping track");
                                warn!("Stopped track");
                                self.track_handle = None;
                            }
                        }
                    }
                    Err(e) => {
                        warn!("Couldn't get track info: {}", e);
                    }
                }
            }
            None => {
                warn!("No Track");
            }
        }
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
                error!("Told to leave but we weren't here");
            }
        }
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
                }
            }
        }
        // If we get here for some reason, return nothing
        warn!("we couldn't find our guy");
        return Err(());
    }

    async fn make_yt_track(&mut self, url: &str) -> Result<Track, Error> {
        // Create our player
        let youtube_input = ytdl(url).await?;
        let metadata = youtube_input.metadata.clone();
        warn!("Loaded up youtube: {} - {}", metadata.title.unwrap(), metadata.source_url.unwrap());
        let (audio, track_handle) = create_player(youtube_input);
        // Give it the handle to end the call if need be
        /*
        track_handle.add_event(
            Event::Track(TrackEvent::End),
            TrackEndCallback {
                //player_data: self.player_data.clone(),
                audio_player: Arc::new(Mutex::new(self.clone())),
                timeout: std::time::Duration::from_secs(30),
            }
        ).expect("Failed to add handle to track");
        */
        // Record our track object
        self.set_track_handle(track_handle);
        return Ok(audio);
        
    }

    fn play_only_track(&mut self, track: Track) {

        // Add the idle event listener to the driver
        self.add_idle_check(); // needs to be before the call lock, because we got the lock in this function

        // Start playing our audio
        match &self.call_handle_lock {
            Some(c) => {
                let mut call = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        c.lock().await
                    })
                });
                // Play our track
                call.play_only(track);
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
                            /*
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
                            */
                        }
                        Err(_) => warn!("Couldn't find our summoner"),
                    }
                }
                else {
                    warn!("We're in a call already");
                }
                // Play the track
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
#[derive(Clone, Debug)]
struct TrackEndCallback {
    audio_player: Arc<Mutex<AudioPlayer>>,
    timeout: std::time::Duration,
}

#[async_trait]
impl SongBirdEventHandler for TrackEndCallback {
    async fn act(&self, _ctx: &EventContext<'_>) -> Option<Event> {
        warn!("Track end callback fired (timeout routine)");
        std::thread::sleep(self.timeout);
        let mut player = self.audio_player.lock().await;
        warn!("Got lock");
        player.hangup();
        warn!("Hung up");

        warn!("{:?}", player);

        return None;
    }
}