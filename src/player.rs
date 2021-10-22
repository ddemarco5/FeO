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
    CoreEvent,
    input::error::Error,
    error::JoinResult,
};

use serenity::{
    CacheAndHttp,
    prelude::*,
    async_trait,
    model::{id::{ChannelId, EmojiId}},
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::channel::{Message, ChannelType, Channel, GuildChannel, ReactionType},
};

// For our url regex matching
use regex::Regex;

#[derive(Clone, Debug)]
enum TrackEndAction {
    NOTHING,
    LEAVE,
    TIMEOUT,
}

#[derive(Clone)]
pub struct AudioPlayer {
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    track_handle: Option<TrackHandle>,
    songbird: Arc<Songbird>,
    idle_callback_action: TrackEndAction,
    idle_callback_struct: Option<TrackEndCallback>,
    timeout_handle: Option<Arc<Mutex<tokio::task::JoinHandle<()>>>>,
    cache_and_http: Option<std::sync::Arc<CacheAndHttp>>,
}


impl AudioPlayer {
    pub async fn new() -> (Arc<Mutex<AudioPlayer>>, AudioPlayerHandler) {
        // The actual player object
        let player = Arc::new(Mutex::new(AudioPlayer {
            call_handle_lock: None,
            track_handle: None,
            songbird: Songbird::serenity(),
            idle_callback_action: TrackEndAction::NOTHING,
            idle_callback_struct: None,
            timeout_handle: None,
            cache_and_http: None,
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
    pub async fn init_player(&mut self, cache_and_http: std::sync::Arc<CacheAndHttp>, shard_count: u64, guild_id_u64: u64) {
        // Save a reference of serenity's cache and http object for later use
        self.cache_and_http = Some(cache_and_http.clone());
        
        let cache_http_clone = cache_and_http.clone();   
        let bot_user_id = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                cache_http_clone.http.get_current_user().await.expect("couldn't get current user").id
            })
        });
        self.songbird.initialise_client_data(shard_count, bot_user_id);
        let guild_id = songbird::id::GuildId::from(guild_id_u64);
        warn!("Trying to create call for guild ID: {}", guild_id);
        let call_lock = self.songbird.get_or_insert(guild_id);
        self.call_handle_lock = Some(call_lock.clone());
        let mut call = call_lock.lock().await;
        // Add the callback to track end event
        call.add_global_event(
            Event::Track(TrackEvent::End),
            // Install a copy of our callback struct as an event, this only needs to ever be done once,
            // as the call actually persists, even if we call leave()
            self.idle_callback_struct.as_ref().unwrap().clone(),
        );
        // Add the callback to client disconnect event
        call.add_global_event(
            Event::Core(CoreEvent::ClientDisconnect),
            self.idle_callback_struct.as_ref().unwrap().clone(),
        );
        warn!("Installed track end event and callback");
        warn!("Created call for guild {}", guild_id);
    }

    pub fn set_track_handle(&mut self, new_handle: TrackHandle) {
        self.track_handle = Some(new_handle);
    }

    pub fn clear_track_handle(&mut self) {
        self.track_handle = None;
    }

    pub fn get_songbird(&self) -> Arc<Songbird> {
        return self.songbird.clone()
    }

    fn set_idle_check(&mut self, action: TrackEndAction) {
        warn!("Setting track end action to {:?}", action);
        self.idle_callback_action = action;
    }


    fn cancel_timeout(&mut self) {
        if let Some(timeout_handle) = &self.timeout_handle.clone() {
            let handle = tokio::task::block_in_place(move || {
                tokio::runtime::Handle::current().block_on(async move {
                    timeout_handle.lock().await
                })
            });
            handle.abort();
            warn!("Aborted existing handle");
            self.timeout_handle = None;
        }
        else {
            warn!("No timeout handle to abort");
        }
    }

    // The reset presence and activity action for both ready and result
    async fn set_status(&self, ctx: &Context) {
        ctx.reset_presence().await;
        ctx.set_activity(Activity::watching("the sniffer")).await;
    }

    pub fn pause(&self) -> Result<(), String> {
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
                                match t.pause() {
                                    Ok(_) => {
                                        warn!("Paused track");
                                    }
                                    Err(_) => {
                                        return Err(String::from("Error pausing track"));
                                    }
                                }
                            }
                            _ => {
                                return Err(String::from("No track playing"));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(String::from(format!("Couldn't get track info: {}", e)));
                    }
                }
            }
            None => {
                return Err(String::from("No Track"));
            }
        }
        Ok(())
    }

    pub fn resume(&self) -> Result<(), String> {
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
                                match t.play() {
                                    Ok(_) => {
                                        warn!("Resumed track");
                                    }
                                    Err(_) => {
                                        return Err(String::from("Error playing track"));
                                    }
                                }
                            }
                            _ => {
                                return Err(String::from("No track paused"));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(String::from(format!("Couldn't get track info: {}", e)));
                    }
                }
            }
            None => {
                return Err(String::from("No Track"));
            }
        }
        Ok(())  
    }

    pub fn stop(&mut self) -> Result<(), String> {
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
                                match t.stop() {
                                    Ok(_) => {
                                        warn!("Stopped track");
                                        self.track_handle = None;
                                    }
                                    Err(_) => {
                                        return Err(String::from("Error stopping track"));
                                    }
                                } 
                            }
                        }
                    }
                    Err(e) => {
                        warn!("No track to stop: {}", e);
                    }
                }
            }
            None => {
                warn!("No Track");
            }
        }
        Ok(())
    }

    pub fn hangup(&mut self) -> Result<(), String> {
        self.stop()?;
        self.clear_track_handle();
        let hangup_result: Result<(), String> = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
                if let Some(_) = call.current_connection() {
                    //call.leave().await.expect("Error leaving call");
                    match call.leave().await {
                        Err(_) => {
                            return Err(String::from("Error leaving call"));
                        }
                        _ => {} // if we succeed, just proceed with program flow
                    }
                }
                else {
                    warn!("Not in a call");
                }
                Ok(())
            })
        });       
        warn!("Hung up");
        return hangup_result;
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.set_idle_check(TrackEndAction::NOTHING);
        self.cancel_timeout();
        self.hangup()?;
        Ok(())
    }

    async fn join_summoner(&mut self, new_message: &Message, ctx: &Context) -> Result<(), ()> {

        let summoner = new_message.author.clone();
        warn!("{} ({}) is summoning", summoner.name, summoner.id);
        // TODO: Can probably use songbird to iterate the voice channels
        let current_guild_id = new_message.guild_id.expect("No guild id in this message");
        let mut voice_channels = current_guild_id.channels(&ctx.http).await.unwrap().values().cloned().collect::<Vec<GuildChannel>>();
        // remove all non-voice channels
        voice_channels.retain(|x| x.kind == ChannelType::Voice);
        // Look for our members
        for channel in voice_channels {
            for member in channel.members(ctx.cache.clone()).await.unwrap() {
                if member.user == summoner {
                    warn!("found our summoner \"{}\" in channel \"{}\"", summoner.name, channel.name);
                    match self.join_channel(&channel).await {
                        Ok(_) => return Ok(()),
                        Err(e) => {
                            error!("Error joining channel {}", e);
                            return Err(());
                        }
                    }
                }
            }
        }
        // If we get here for some reason, return nothing
        warn!("we couldn't find our guy");
        return Err(());
    }

    async fn join_most_crowded(&mut self, new_message: &Message, ctx: &Context) -> Result<(), ()> {

        // TODO: Can probably use songbird to iterate the voice channels
        let current_guild_id = new_message.guild_id.expect("No guild id in this message");
        let mut voice_channels = current_guild_id.channels(&ctx.http).await.unwrap().values().cloned().collect::<Vec<GuildChannel>>();
        // remove all non-voice channels
        voice_channels.retain(|x| x.kind == ChannelType::Voice);
        // sort channels by most to least crowded
        voice_channels.sort_by(
            |a, b| {
                let a_members = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        a.members(ctx.cache.clone()).await.unwrap().len()
                    })
                });
                let b_members = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        b.members(ctx.cache.clone()).await.unwrap().len()
                    })
                });
                b_members.partial_cmp(&a_members).unwrap()
            }
        );
        // If the first (most crowded) voice channel has no members
        if voice_channels.first().unwrap().members(ctx.cache.clone()).await.unwrap().len() > 0 {
            match voice_channels.first() {
                Some(c) => {
                    warn!("Joining most crowded channel {}", c.name);
                    match self.join_channel(c).await {
                        Ok(_) => return Ok(()),
                        Err(e) => {
                            error!("Error joining channel {}", e);
                            return Err(());
                        }
                    }
                }
                None => {
                    warn!("No voice channels");
                    return Err(());
                }
                
            } 
        }
        else {
            warn!("Nobody in any of the voice channels");
            return Err(());
        }
    }

    async fn join_channel(&mut self, channel: &GuildChannel) -> JoinResult<()> {

        let songbird_channel_id = songbird::id::ChannelId::from(channel.id);
        let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
        match call.current_connection() {
            Some(i) => {
                // Songbird channel id vs serenity channel id. Unwrap them both down to their u64s
                if i.channel_id.unwrap() == songbird_channel_id {
                    warn!("We're already in this channel");
                }
                else {
                    warn!("In a different channel, joining a new one");
                }
            }
            None => {
                warn!("Not in a channel");
            }
        }
        warn!("bitrate is {}", channel.bitrate.unwrap());
        let bitrate = Bitrate::BitsPerSecond(channel.bitrate.unwrap() as i32);
         // Set our call's bitrate
        call.set_bitrate(bitrate);
        // Join the channel
        call.join(songbird_channel_id).await?; //the ? will propegate
        return Ok(());
    }

    async fn make_ytdl_track(&mut self, url: &str) -> Result<Track, Error> {
        warn!("Loading url: {}", url);
        // Create our player
        let youtube_input = ytdl(url).await?;
        let metadata = youtube_input.metadata.clone();
        warn!("Loaded up track: {} - {}", metadata.title.unwrap(), metadata.source_url.unwrap());
        let (audio, track_handle) = create_player(youtube_input);
        // Give it the handle to end the call if need be

        // Record our track object
        self.set_track_handle(track_handle);
        return Ok(audio);
    }

    fn play_only_track(&mut self, track: Track) {
        // Start playing our audio
        let mut call = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                self.call_handle_lock.as_ref().unwrap().lock().await
            })
        });
        // Play our track
        call.play_only(track);
    }

    fn parse_url(&self, message: &Message) -> Result<String, ()> {
        lazy_static! {
            // Returns the whole string to replace in the first capture, contents of [] in 2nd and () in 3rd
            //static ref RE: Regex = Regex::new(r"https://\S*youtu\S*").unwrap();
            static ref RE: Regex = Regex::new(r"https://\S*").unwrap();
        }

        match RE.captures(message.content.as_str()) {
            None => {
                error!("regex failed to match url");
                return Err(());
            }
            Some(r) => {
                return Ok(String::from(&r[0]));
            }
        }
    }

    async fn process_driveby(&mut self, ctx: &Context, new_message: &Message) -> Result<(), String> {
        match self.parse_url(&new_message) {
            Err(()) => {
                return Err(String::from("Told to driveby, but nothing given"));
            }
            Ok(r) => {
                let url_to_play = r.as_str();
                warn!("driveby with {}", url_to_play);
                // Load up our song
                let track = self.make_ytdl_track(url_to_play).await;
                match track {
                    Ok(t) => {
                        warn!("Successfully loaded track, pullin up");
                        // Join channel with the most people
                        match self.join_most_crowded(&new_message, &ctx).await {
                            Ok(_) => {
                                // Get out of there when we're done
                                self.set_idle_check(TrackEndAction::LEAVE);
                                // play our track
                                self.play_only_track(t);
                                //react_driveby(&ctx, &new_message);
                            }
                            Err(_) => {
                                return Err(String::from("Couldn't find a channel with anyone in it"));
                            }
                        }
                    }
                    Err(e) => {
                        return Err(String::from(format!("Couldn't create youtube track: {}", e)));
                    }
                }
            }
        }
        Ok(())
    }

    async fn process_play(&mut self, ctx: &Context, new_message: &Message) -> Result<(), String> {

        match self.parse_url(&new_message) {
            Err(()) => {
                return Err(String::from("told to play, but nothing given"));
            }
            Ok(r) => {
                let url_to_play = r.as_str();
                warn!("Told to play {}", url_to_play);
                match self.join_summoner(&new_message, &ctx).await {
                    Ok(_) => {
                        warn!("Joined summoner");
                        // Remove the timeout so we don't accidentally hang up while we process
                        self.cancel_timeout();
                        // Play the track
                        let track = self.make_ytdl_track(url_to_play).await;
                        match track {
                            Ok(t) => {
                                warn!("Successfully created track, playing");
                                // Add the idle event listener to the driver
                                self.set_idle_check(TrackEndAction::TIMEOUT);
                                // play our track
                                self.play_only_track(t);
                            }
                            Err(e) => {
                                // Leave bc we can't play shit
                                self.hangup()?;
                                return Err(String::from(format!("Couldn't create youtube track: {}", e)));
                            }
                        }
                    }
                    Err(_) => {
                        return Err(String::from("Couldn't find our summoner"));
                    }
                }
                Ok(())
            }
        }
    }
}

pub struct AudioPlayerHandler {
    audio_player: Arc<Mutex<AudioPlayer>>,
}

impl AudioPlayerHandler {
    async fn handle_command(&self, ctx: &Context, new_message: &Message) -> Result<(), String> {
        match new_message.content.as_str() {
            "leave" => {
                warn!("Told to leave");
                let mut player = self.audio_player.lock().await;
                player.hangup()?;
                return Ok(());
            }
            "stop" => {
                warn!("Told to stop");
                let mut player = self.audio_player.lock().await;
                player.stop()?;
                return Ok(());
            }
            "pause" => {
                warn!("Told to pause");
                let player = self.audio_player.lock().await;
                player.pause()?;
                return Ok(());
            }
            "resume" => {
                warn!("Told to resume");
                let player = self.audio_player.lock().await;
                player.resume()?;
                return Ok(());
            }
            // Do our play matching below because "match" doesn't play well with contains
            _ => {
                if new_message.content.contains("play") {
                    let mut player = self.audio_player.lock().await;
                    player.process_play(&ctx, &new_message).await?;
                    return Ok(());
                }
                else if new_message.content.contains("driveby") {
                    let mut player = self.audio_player.lock().await;
                    player.process_driveby(&ctx, &new_message).await?;
                    return Ok(());
                }
                //else {
                //    error!("We got a message here, but it isn't any we are interested in");
                //}
            }
        }
        return Err(String::from("No valid command found in message"));
    }
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
            match self.handle_command(&ctx, &new_message).await {
                Ok(_) => {
                    react_success(&ctx, &new_message);
                }
                Err(e) => {
                    error!("{}", e);
                    react_fail(&ctx, &new_message);
                }
            }
        }
    }
}

// Very specific struct only for the purpose of leaving the call if nothing is playing after an idle timeout
#[derive(Clone)]
struct TrackEndCallback {
    audio_player: Arc<Mutex<AudioPlayer>>,
    timeout: std::time::Duration,
}


// Multi-use callback, installed in track end events and whatever other cases I want to write in
#[async_trait]
impl SongBirdEventHandler for TrackEndCallback {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        warn!("Running track end handler");
        match ctx {
            EventContext::Track(_) => {
                warn!("Got track event");
                let mut player = self.audio_player.lock().await;
                match &player.idle_callback_action {
                    // Timeout the call after inactivity
                    TrackEndAction::TIMEOUT => {
                        // If we have an existing handle, abort it to start again
                        if let Some(timeout_handle) = player.timeout_handle.clone() {
                            let handle = timeout_handle.lock().await;
                            handle.abort();
                            warn!("Aborted existing handle");
                        }
                        // Spawn our thread to wait our timeout amount
                        // clone our stuff for use in task
                        let player_clone = self.audio_player.clone();
                        let timeout = self.timeout.clone();
                        player.timeout_handle = Some(Arc::new(Mutex::new(tokio::spawn(async move {
                            tokio::time::sleep(timeout).await; // We use tokio's sleep because it's abortable
                            warn!("Reached our timeout");
                            let mut player = player_clone.lock().await;
                            player.shutdown().unwrap();
                            warn!("shut down our player");
                        }))));
                        warn!("spawned tokio timeout task");
                    }
                    // Leave immediately
                    TrackEndAction::LEAVE => {
                        warn!("Leaving the call");
                        player.shutdown().unwrap();
                    }
                    // Don't do anything, let it end and sit there
                    TrackEndAction::NOTHING => {
                        warn!("Do nothing, idle check is disabled")
                    }
                }
            }
            // Leave if the channel is empty after a disconnect
            EventContext::ClientDisconnect(_) => {
                warn!("Client disconnect event");
                // We do this in this scoped fashion so we drop the lock after we pull the channel id and cache
                let (current_channel_id_u64, cache_and_http) = {
                    let player = self.audio_player.lock().await;
                    let call = player.call_handle_lock.as_ref().unwrap().lock().await;
                    (call.current_channel().unwrap().0, player.cache_and_http.clone())
                };
                let serenity_channel_id = ChannelId::from(current_channel_id_u64);
                // Get the channel members
                if let Some(x) = cache_and_http {
                    let cache = x.cache.clone();
                    let channel = serenity_channel_id.to_channel_cached(cache.clone()).await.expect("couldn't find channel");
                    // If it's a guild channel
                    match channel {
                        Channel::Guild(c) => {
                            // Pretty stupid, but sometimes the members list reports the user that just left
                            // so wait a second for discord to properly register this person as gone
                            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                            let members = c.members(cache).await.expect("Error checking members in channel");
                            if members.len() > 1 { // 1 because the sniffer will be in this channel
                                warn!("Still members in the channel, staying");
                            }
                            else {    
                                warn!("No more members in the channel, stopping");
                                let mut player = self.audio_player.lock().await;
                                player.hangup().unwrap();
                            }
                        }
                        _ => {
                            warn!("not a guild channel");
                        }
                    }

                }
            }
            _ => {
                warn!("Some event {:?}, we don't care about it", ctx);
            }
        }
        
        return None;
    }
}

fn react_success(ctx: &Context, message: &Message) {
    tokio::task::block_in_place(move || {
        tokio::runtime::Handle::current().block_on(async move {
            message.react(ctx.http.clone(), ReactionType::Custom{
                animated: false,
                id: EmojiId(801166698610294895),
                name: Some(String::from(":guthchamp:")),
            }).await.expect("Failed to react to post");
        })
    });
}

fn react_fail(ctx: &Context, message: &Message) {
    tokio::task::block_in_place(move || {
        tokio::runtime::Handle::current().block_on(async move {
            message.react(ctx.http.clone(), ReactionType::Custom{
                animated: false,
                id: EmojiId(886356280934006844),
                name: Some(String::from(":final_pepe:")),
            }).await.expect("Failed to react to post");
        })
    });
}