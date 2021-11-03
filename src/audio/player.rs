use std::sync::{Arc};
use tokio::sync::{Mutex};

use songbird::{
    {Songbird, Call},
    {ytdl, tracks::create_player},
    tracks::{Track, PlayMode},
    driver::Bitrate,
    Event,
    EventContext,
    EventHandler as SongBirdEventHandler,
    TrackEvent,
    CoreEvent,
    input::error::Error,
    error::JoinResult,
    Config,
};

use serenity::{
    CacheAndHttp,
    prelude::*,
    async_trait,
    model::{id::{ChannelId}},
    model::channel::{Message, ChannelType, Channel, GuildChannel},
};

use uuid::Uuid;
use crate::commands::Token;

static HELP_TEXT: &str =
"```\n\
help - show this\n\
play 'url' - plays the given url, inserts into the front of the queue\n\
driveby 'url' - driveby a channel with the given url\n\
queue 'url' * - queue up the given url(s), starts playing if queue was empty\n\
next 'url' - queue up the given url to play next\n\
goto X (>0) - jump to and play the queue index given\n\
rm X Y, etc (>0) - remove queue elements, provide indices separated by spaces\n\
list - lists the current queue\n\
pause - pause currently playing track\n\
resume - resume a currently pause track\n\
skip - skip the current track\n\
clear - clears everything in the queue but the song playing \n\
stop - stop the player, but don't leave\n\
leave - tells the player to fuck outta here\n\
```\
";

// macro to break our tokio lock out of async
macro_rules! lock_call {
    ($a:expr) => {
        {
            tokio::task::block_in_place(move || {
                tokio::runtime::Handle::current().block_on(async move {
                    $a.as_ref().unwrap().lock().await
                })
            })
        }
    };
}
macro_rules! lock_call_async {
    ($a:expr) => {
        {
                    $a.as_ref().unwrap().lock().await
        }
    };
}

#[derive(Clone, Debug)]
enum TrackEndAction {
    LEAVE,
    TIMEOUT,
}

#[derive(Clone)]
pub struct AudioPlayer {
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    songbird: Arc<Songbird>,
    idle_callback_action: TrackEndAction,
    idle_callback_struct: Option<TrackEndCallback>,
    timeout_handle: Option<Arc<Mutex<tokio::task::JoinHandle<()>>>>,
    cache_and_http: Option<std::sync::Arc<CacheAndHttp>>,
    pub audio_text_channel: ChannelId,
}


impl AudioPlayer {
    pub async fn new(audio_channel: u64, queue_size: usize, timeout: std::time::Duration) -> Arc<Mutex<AudioPlayer>> {
        // The actual player object
        let player = Arc::new(Mutex::new(AudioPlayer {
            call_handle_lock: None,
            songbird: Songbird::serenity_from_config(
                Config::default().preallocated_tracks(queue_size)
            ),
            idle_callback_action: TrackEndAction::TIMEOUT,
            idle_callback_struct: None,
            timeout_handle: None,
            cache_and_http: None,
            audio_text_channel: ChannelId(audio_channel),
        }));
        
        // Get the lock on our player so we can modify it
        {
            let mut player_locked = player.lock().await;
            
            // Create the callback structure
            player_locked.idle_callback_struct = Some(TrackEndCallback {
                audio_player: player.clone(),
                timeout: timeout,
            });
        }    
        return player;
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

    pub fn pause_locking(&self) -> Result<(), String> {
        let mut call = lock_call!(self.call_handle_lock);
        self.pause(&mut call)
    }
    fn pause(&self, call: &mut Call) -> Result<(), String> {
        match call.queue().pause() {
            Ok(_) => {
                warn!("Paused track");
            }
            Err(e) => {
                return Err(String::from(format!("Error pausing track: {}", e)));
            }
        }
        Ok(())
    }

    pub fn resume_locking(&self) -> Result<(), String> {
        let mut call = lock_call!(self.call_handle_lock);
        self.resume(&mut call)
    }
    fn resume(&self, call: &mut Call) -> Result<(), String> {
        match call.queue().resume() {
            Ok(_) => {
                warn!("Resumed track");
            }
            Err(e) => {
                return Err(String::from(format!("Error resuming track: {}", e)));
            }
        }
        Ok(())
    }

    /// Stops the player and clears the queue
    pub fn stop_locking(&self) -> Result<(), String> {
        let mut call = lock_call!(self.call_handle_lock);
        self.stop(&mut call)
    }
    fn stop(&self, call: &mut Call) -> Result<(), String> {
        call.queue().stop();
        Ok(())
    }
    
    pub fn skip_locking(&self) -> Result<(), String> {
        let mut call = lock_call!(self.call_handle_lock);
        self.skip(&mut call)
    }
    fn skip(&self, call: &mut Call) -> Result<(), String> {
        match call.queue().skip() {
            Ok(_) => {
                warn!("Skipping track");
            }
            Err(e) => {
                return Err(String::from(format!("Error skipping track: {}", e)));
            }
        }
        Ok(())
    }

    pub fn hangup(&mut self) -> Result<(), String> {
        //self.clear_track_handle();
        let hangup_result: Result<(), String> = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
                // full stop the queue
                call.queue().stop();
                if let Some(_) = call.current_connection() {
                    if let Err(_) = call.leave().await {
                        return Err(String::from("Error leaving call"));
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
        self.cancel_timeout();
        self.hangup()?;
        Ok(())
    }

    async fn join_summoner(&mut self, new_message: &Message, ctx: &Context) -> Result<(), String> {

        let summoner = new_message.author.clone();
        warn!("{} ({}) is summoning", summoner.name, summoner.id);
        // TODO: Can probably use songbird to iterate the voice channels
        let current_guild_id = match new_message.guild_id {
            Some(id) => id,
            None => {
                return Err(String::from("No guild id in this message"));
            }   
        };

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
                            return Err(String::from(format!("Error joining channel {}", e)));
                        }
                    }
                }
            }
        }
        // If we get here for some reason, return nothing
        return Err(String::from("we couldn't find our guy"));
    }

    async fn join_most_crowded(&mut self, new_message: &Message, ctx: &Context) -> Result<(), String> {

        // TODO: Can probably use songbird to iterate the voice channels
        let current_guild_id = match new_message.guild_id {
            Some(id) => id,
            None => {
                return Err(String::from("No guild id in this message"));
            }   
        };
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
                            return Err(String::from(format!("Error joining channel {}", e)));
                        }
                    }
                }
                None => {
                    return Err(String::from("No voice channels"));
                }
                
            } 
        }
        else {
            return Err(String::from("Nobody in any of the voice channels"));
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
        let (audio, _track_handle) = create_player(youtube_input);
        // Give it the handle to end the call if need be
        // Record our track object
        return Ok(audio);
    }

    async fn play_only_track(&mut self, track: Track) -> Result<(), String> {

        // Get our call lock
        let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
        // Queue up our new track
        call.enqueue(track);

        let queue = call.queue().clone();

        // If we have more than 1 elements now
        if queue.len() > 1 {
            // Due to limitations of the library, we can't stop and restart, we must pause
            self.pause(&mut call)?;
            drop(call); // drop our lock so we can cancel timeout
            // There's a chance the timeout triggers when we're loading a track, this fixes that
            self.cancel_timeout();
            // and move new track to the frount of the queue.
            queue.modify_queue(
                |q| {
                    // pop our track from the back and add it to the front
                    let new_track = q.pop_back().unwrap();
                    q.push_front(new_track);
                }
            );
        }
        // Now play the track and the front of our queue
        match queue.resume() {
            Ok(_) => {
                warn!("Playing new track");
            }
            Err(e) => {
                return Err(String::from(format!("Error playing new track: {}", e)));
            }
        }

 
        Ok(())
    }


    pub async fn process_driveby(&mut self, ctx: &Context, new_message: &Message, args: Vec<Token>) -> Result<(), String> {
        if args.len() > 1 {
            return Err(String::from("Too many arguments given to driveby"));
        }
        if let Token::Generic(url_to_play) = args.first().unwrap() {
            warn!("Told to play {}", url_to_play);
            // Remove the timeout so we don't accidentally hang up while we process
            self.cancel_timeout();
            // Play the track
            warn!("driveby with {}", url_to_play);
            // Load up our song
            let track = match self.make_ytdl_track(url_to_play).await {
                Ok(t) => t,
                Err(e) => {
                    return Err(String::from(format!("Error making yt track: {}", e)));
                }
            };
            warn!("Successfully loaded track, pullin up");
            // Join channel with the most people

            self.join_most_crowded(&new_message, &ctx).await?;
            // Get out of there when we're done
            self.set_idle_check(TrackEndAction::LEAVE);
            // play our track
            self.play_only_track(track).await?;
            // Clear the queue after we join
            self.clear_queue_locking()?;
        }
        Ok(())
    }

    pub async fn process_play(&mut self, ctx: &Context, new_message: &Message, args: Vec<Token>) -> Result<(), String> {

        if args.len() > 1 {
            return Err(String::from("Too many arguments given to play"));
        }
        if let Token::Generic(url_to_play) = args.first().unwrap() {
            warn!("Told to play {}", url_to_play);
            // Remove the timeout so we don't accidentally hang up while we process
            self.cancel_timeout();
            // Play the track
            let track = self.make_ytdl_track(url_to_play.as_str()).await;
            match track {
                Ok(t) => {
                    warn!("Successfully created track");
                    // Make sure our idle action is set to timeout
                    self.set_idle_check(TrackEndAction::TIMEOUT);
                    self.join_summoner(&new_message, &ctx).await?;
                    warn!("Joined summoner");
                    // play our track
                    warn!("playing");
                    self.play_only_track(t).await?;
                }
                Err(e) => {
                    // Leave bc we can't play shit
                    return Err(String::from(format!("Couldn't create track: {}", e)));
                }
            }
        } 
        Ok(())
    }

    pub async fn process_enqueue(&mut self, ctx: &Context, new_message: &Message, args: Vec<Token>) -> Result<(), String> {

        if args.is_empty() {
            return Err(String::from("told to queue, but nothing given"));
        }

        let mut tracks = Vec::<Track>::new();
        for url_to_play in args {
            match url_to_play {
                Token::Generic(url) => {
                    warn!("Told to queue {}", url);
                    // Make the track
                    match self.make_ytdl_track(url.as_str()).await {
                        Ok(t) => {
                            warn!("Successfully created track");
                            tracks.push(t);
                        }
                        Err(e) => {
                            return Err(String::from(format!("Couldn't create track: {}", e)));
                        }
                    }
                }
                _ => {
                    return Err(String::from("given invalid token to play"));
                }
            }
        }
        // If we get here it means we have a vec of successfully created tracks
        //Join the call
        self.join_summoner(&new_message, &ctx).await?;
        warn!("Joined summoner");
        // Make sure our idle action is set to timeout
        self.set_idle_check(TrackEndAction::TIMEOUT);
        let mut call = lock_call_async!(self.call_handle_lock);
        for track in tracks {
            call.enqueue(track);
            warn!("Queued track");
        }
        Ok(())
    }

    pub async fn process_next(&mut self, ctx: &Context, new_message: &Message, args: Vec<Token>) -> Result<(), String> {
        let call_handle_lock = self.call_handle_lock.clone();
        let queue = {
            //let call = self.call_handle_lock.as_ref().unwrap().lock().await;
            let call = lock_call_async!(self.call_handle_lock);
            call.queue().clone()
        };
       
        match queue.is_empty() {
            true => {
                warn!("queue is empty, just load a basic track");
                self.process_play(ctx, new_message, args).await?;
            }
            false => {
                if let Token::Generic(url_to_play) = args.first().unwrap() {
                    warn!("Told to queue next {}", url_to_play);
                    // Make the track
                    let track = self.make_ytdl_track(url_to_play).await;
                    match track {
                        Ok(t) => {
                            warn!("Successfully created track");
                            // Queue up the track, and rearrange it so it'll come after what's currently playing
                            let mut call = lock_call_async!(call_handle_lock);
                            call.enqueue(t);
                            call.queue().modify_queue(
                                |q| {
                                    // pop our track from the back and set it to be the next track
                                    let new_track = q.pop_back().unwrap();
                                    q.insert(1, new_track);
                                }
                            );
                        }
                        Err(e) => {
                            return Err(String::from(format!("Couldn't create track: {}", e)));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub async fn process_rm(&mut self, args: Vec<Token>) -> Result<(), String> {
        
        //let indices_to_rm = self.parse_rm(new_message)?;

        let mut indices_to_rm = Vec::<usize>::new();
        for idx in args {
            match idx {
                Token::Generic(s) => {
                    match s.parse::<u32>() {
                        Ok(idx) => indices_to_rm.push(idx as usize),
                        Err(e) => return Err(String::from(format!("Couldn't parse index from argument: {}", e))),
                    }
                }
                _ => {
                    return Err(String::from("Invalid token"));
                }
            }
        }

        // validate that none of our removals are larger than our playlist
        let playlist_len = {
            let call = self.call_handle_lock.as_ref().unwrap().lock().await;
            call.queue().len()
        };
        if playlist_len == 0 {
            return Err(String::from("Empty playlist"));
        }
        for ind in &indices_to_rm {
            // If our index is out of range or 0, the currently playing track
            if (*ind > playlist_len-1) || (*ind < 1 ) {
                return Err(String::from(format!("Index {} is invalid", ind)));
            }
        }

        // Remove desired indices
        let call = self.call_handle_lock.as_ref().unwrap().lock().await;
        call.queue().modify_queue(
            |q| {
                let mut removalvec: Vec<Uuid> = Vec::new();
                // Get our Queued objects we want to delete based on their source urls
                for (i, item) in q.iter().enumerate() {
                    if indices_to_rm.contains(&i){
                        removalvec.push(item.uuid());
                        // Stop the track in case it happens to be playing
                        if let Err(e) = item.stop() {
                            return Err(String::from(format!("Track failed to stop playing: {}", e)));
                        }
                        warn!("Stopped track before queue removal");
                    }
                }
                // Retain everything we don't want to remove
                q.retain(|track| !removalvec.contains(&track.uuid()));
                Ok(())
            }
        )?;

        Ok(())
    }

    pub async fn process_goto(&self, args: Vec<Token>) -> Result<(), String> {
        // Process the goto command, but there's a trick... because of how we structure our queue,
        // all we actually have to do is skip an equal amount of times as the track index we're given
        //let idx = self.parse_goto(new_message)?;
        let idx = match args.first().unwrap() {
            Token::Generic(s) => {
                //let idx = s.parse::<u32>();
                match s.parse::<u32>() {
                    Ok(idx) => idx,
                    Err(e) => return Err(String::from(format!("Couldn't parse index from argument: {}", e))),
                }
            }
            _ => {
                return Err(String::from("Invalid token given"));
            }
        };
        // make sure we've got some values that make sense for this function
        if idx < 1 {
            return Err(String::from("Tried to go to less than 1"));
        }
        // validate that none of our removals are larger than our playlist
        let playlist_len = {
            let call = self.call_handle_lock.as_ref().unwrap().lock().await;
            call.queue().len()
        };
        if playlist_len == 0 {
            return Err(String::from("Empty playlist"));
        }
        if idx as usize > playlist_len-1 {
            return Err(String::from(format!("Index {} is invalid", idx)));
        }
        // Stop our current track
        let call = self.call_handle_lock.as_ref().unwrap().lock().await;
        if let Some(t) = call.queue().current() {
            if let Err(e) = t.stop() {
                return Err(String::from(format!("Error stopping track: {}", e)));
            }
        }
        // Remove up to our index
        call.queue().modify_queue(
            |q| {
                for _ in 0..idx {
                    if let Some(t) = q.pop_front() {  // remove our track from the queue
                        // If we got a track from the pop, stop it to avoid any memory leaks
                        if let Err(e) = t.stop() {
                            return Err(String::from(format!("Error stopping track in queue removal: {}", e)));
                        }
                    } 
                }
                Ok(())
            }
        )?;

        match call.queue().resume() {
            Ok(_) => warn!("Went to track, playing"),
            Err(e) => return Err(String::from(format!("Error starting track after goto: {}", e))),
        }
        Ok(())
    }

    /// Remove all the tracks except the one currently playing
    pub fn clear_queue_locking(&self) -> Result<(), String> {
        let mut call = lock_call!(self.call_handle_lock);
        self.clear_queue(&mut call)
    }
    fn clear_queue(&self, call: &Call) -> Result<(), String> {

        if call.queue().is_empty() {
            return Err(String::from("Queue is empty, can't clear shit"));
        }

        // Remove up to our index
        call.queue().modify_queue(
            |q| {
                // A this point we know the queue isn't empty, so go ahead and drain
                q.drain(1..);
            }
        );
        warn!("Cleared queued tracks");
        Ok(())
    }

    pub fn print_help(&self, ctx: &Context) -> Result<(), String> {
        // Print a help message to the audio text channel
        let send_result = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                self.audio_text_channel.say(ctx.http.clone(), HELP_TEXT).await
            })
        });
        match send_result {
            Ok(_) => {
                warn!("Sent help text");
                return Ok(());
            }
            Err(e) => {
                return Err(String::from(format!("Failed to send help text: {}", e)));
            }
        };
    }

    pub fn print_queue(&self, ctx: &Context) -> Result<(), String> {
        let call = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                self.call_handle_lock.as_ref().unwrap().lock().await
            })
        });
        let queue = call.queue().current_queue();
        let mut track_list = String::from("```\n");

        match queue.is_empty() {
            true => {
                return Err(String::from("Queue is empty"));
            }
            false => {
                for (i, track) in queue.iter().enumerate() {
                    let metadata = track.metadata();
                    let mut track_string = String::new();
                    if i == 0 { // If we're at index 0, that's what we're currently playing
                        track_string.push_str(">>> ");
                    }
                    else { // Otherwise we're actually a track index
                        track_string.push_str(format!("{} - ", i).as_str());
                    }
                    match &metadata.track {
                        Some(t) => {
                            track_string.push_str(format!("{}", t).as_str());
                        }
                        None => {
                            track_string.push_str(format!("{}", metadata.title.as_ref().unwrap()).as_str());
                        }
                    }
                    if let Some(x) = &metadata.artist { 
                        track_string.push_str(format!(", {}", x).as_str());
                    }
                    if let Some(x) = &metadata.duration {
                        track_string.push_str(format!(", {:#?}\n", x).as_str());
                    }
                    track_list.push_str(track_string.as_str());
                }
                track_list.push_str("```");
                let send_result = tokio::task::block_in_place(move || {
                    tokio::runtime::Handle::current().block_on(async move {
                        self.audio_text_channel.say(ctx.http.clone(), track_list).await
                    })
                });
                match send_result {
                    Ok(_) => {
                        warn!("Sent track list");
                        return Ok(());
                    }
                    Err(e) => {
                        return Err(String::from(format!("Failed to send track list: {}", e)));
                    }
                };
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
                            // Check to make sure we're not currently playing a song or our queue is empty
                            let queue = { // Do this in a closure so we drop the call lock when done
                                let call = player.call_handle_lock.as_ref().unwrap().lock().await;
                                call.queue().clone()
                            };
                            if !queue.is_empty() {
                                if let Some(h) = queue.current() {
                                    match h.get_info().await {
                                        Ok(s) => {
                                            if s.playing == PlayMode::Play {
                                                warn!("Still playing a track, not going to shutdown");
                                            }
                                        }
                                        Err(e) => {
                                            error!("Error getting track state, probably ended, shutting down: {}", e);
                                            player.shutdown().unwrap();
                                        }
                                    }
                                }
                            }
                            else {
                                player.shutdown().unwrap();
                                warn!("Queue was empty, shutting down player");
                            }  
                        }))));
                        warn!("spawned tokio timeout task");
                    }
                    // Leave immediately
                    TrackEndAction::LEAVE => {
                        warn!("Leaving the call");
                        player.shutdown().unwrap();
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