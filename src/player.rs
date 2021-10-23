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
    model::{id::{ChannelId, EmojiId}},
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::channel::{Message, ChannelType, Channel, GuildChannel, ReactionType},
};

use uuid::Uuid;

static HELP_TEXT: &str =
"```\n\
help - show this\n\
play 'url' - plays the given url, inserts into the front of the queue\n\
driveby 'url' - driveby a channel with the given url\n\
queue 'url' - queue up the given url, starts playing if queue was empty\n\
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

// For our url regex matching
use regex::Regex;

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
    audio_text_channel: ChannelId,
}


impl AudioPlayer {
    pub async fn new(audio_channel: u64, queue_size: usize, timeout: std::time::Duration) -> (Arc<Mutex<AudioPlayer>>, AudioPlayerHandler) {
        // The actual player object
        let player = Arc::new(Mutex::new(AudioPlayer {
            call_handle_lock: None,
            //songbird: Songbird::serenity(),
            songbird: Songbird::serenity_from_config(
                Config::default().preallocated_tracks(queue_size)
            ),
            idle_callback_action: TrackEndAction::TIMEOUT,
            idle_callback_struct: None,
            timeout_handle: None,
            cache_and_http: None,
            audio_text_channel: ChannelId(audio_channel),
        }));
        // The player's event handler
        let handler = AudioPlayerHandler{
            audio_player: player.clone(),
            audio_text_channel: ChannelId(audio_channel), // Keep a copy of the text channel in there
        };
        // Create the callback structure
        {
            let mut player_locked = player.lock().await;

            player_locked.idle_callback_struct = Some(TrackEndCallback {
                audio_player: player.clone(),
                timeout: timeout,
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

    pub fn pause(&self, call: &mut Call) -> Result<(), String> {
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

    pub fn resume(&self, call: &mut Call) -> Result<(), String> {
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
    pub fn stop(&self, call: &mut Call) -> Result<(), String> {
        call.stop();
        Ok(())
    }
    

    pub fn skip(&self, call: &mut Call) -> Result<(), String> {
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
        //self.set_idle_check(TrackEndAction::NOTHING);
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
        //self.set_track_handle(track_handle);
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

    // TODO: this shit, but better
    fn parse_rm(&self, message: &Message) -> Result<Vec<usize>, String> {
        let numbers = message.content.replace("rm ", "");
        let spliterator = numbers.split(" ");
        let mut num_vec: Vec<usize> = Vec::new();
        for num_str in spliterator {
            match num_str.parse::<usize>() {
                Ok(num) => num_vec.push(num),
                Err(e) => {
                    return Err(String::from(format!("Error parsing rm numbers: {}", e)));
                }
            }
        }
        return Ok(num_vec);
    }

    fn parse_goto(&self, message: &Message) -> Result<u32, String> {
        let numbers = message.content.replace("goto ", "");
        match numbers.parse::<u32>() {
            Ok(num) => return Ok(num),
            Err(e) => return Err(String::from(format!("Error parsing goto: {}", e))),
        };
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
                // Remove the timeout so we don't accidentally hang up while we process
                self.cancel_timeout();
                // Play the track
                let track = self.make_ytdl_track(url_to_play).await;
                match track {
                    Ok(t) => {
                        warn!("Successfully created track");
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
                Ok(())
            }
        }
    }

    async fn process_enqueue(&mut self, ctx: &Context, new_message: &Message) -> Result<(), String> {
        match self.parse_url(&new_message) {
            Err(()) => {
                return Err(String::from("told to queue, but nothing given"));
            }
            Ok(r) => {
                let url_to_play = r.as_str();
                warn!("Told to queue {}", url_to_play);
                // Make the track
                let track = self.make_ytdl_track(url_to_play).await;
                match track {
                    Ok(t) => {
                        warn!("Successfully created track");
                        self.join_summoner(&new_message, &ctx).await?;
                        warn!("Joined summoner");
                        let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
                        call.enqueue(t);
                        warn!("Queued up track");
                    }
                    Err(e) => {
                        return Err(String::from(format!("Couldn't create track: {}", e)));
                    }
                }
                Ok(())
            }
        }
    }

    async fn process_next(&mut self, ctx: &Context, new_message: &Message) -> Result<(), String> {
        let queue = {
            let call = self.call_handle_lock.as_ref().unwrap().lock().await;
            call.queue().clone()
        };
       
        match queue.is_empty() {
            true => {
                warn!("queue is empty, just load a basic track");
                self.process_play(ctx, new_message).await?;
            }
            false => {
                match self.parse_url(&new_message) {
                    Err(()) => {
                        return Err(String::from("told to queue next, but nothing given"));
                    }
                    Ok(r) => {
                        let url_to_play = r.as_str();
                        warn!("Told to queue next {}", url_to_play);
                        // Make the track
                        let track = self.make_ytdl_track(url_to_play).await;
                        match track {
                            Ok(t) => {
                                warn!("Successfully created track");
                                // Queue up the track, and rearrange it so it'll come after what's currently playing
                                let mut call = self.call_handle_lock.as_ref().unwrap().lock().await;
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
        }
        Ok(())
    }

    async fn process_rm(&mut self, new_message: &Message) -> Result<(), String> {
        
        let indices_to_rm = self.parse_rm(new_message)?;

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
                        warn!("Adding {:?} to remove list", item);
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

    async fn process_goto(&self, new_message: &Message) -> Result<(), String> {
        // Process the goto command, but there's a trick... because of how we structure our queue,
        // all we actually have to do is skip an equal amount of times as the track index we're given
        let idx = self.parse_goto(new_message)?;
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

    fn print_help(&self, ctx: &Context) -> Result<(), String> {
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

    fn print_queue(&self, ctx: &Context) -> Result<(), String> {
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

pub struct AudioPlayerHandler {
    audio_player: Arc<Mutex<AudioPlayer>>,
    audio_text_channel: ChannelId
}

impl AudioPlayerHandler {
    async fn handle_command(&self, ctx: &Context, new_message: &Message) -> Result<(), String> {
        match new_message.content.as_str() {
            "help" => {
                warn!("Asked to print help text");
                let player = self.audio_player.lock().await;
                player.print_help(&ctx)?;
                return Ok(());
            }
            "leave" => {
                warn!("Told to leave");
                let mut player = self.audio_player.lock().await;
                player.hangup()?;
                return Ok(());
            }
            "stop" => {
                warn!("Told to stop");
                let player = self.audio_player.lock().await;
                let mut call = player.call_handle_lock.as_ref().unwrap().lock().await;
                player.stop(&mut call)?;
                return Ok(());
            }
            "pause" => {
                warn!("Told to pause");
                let player = self.audio_player.lock().await;
                let mut call = player.call_handle_lock.as_ref().unwrap().lock().await;
                player.pause(&mut call)?;
                return Ok(());
            }
            "resume" => {
                warn!("Told to resume");
                let player = self.audio_player.lock().await;
                let mut call = player.call_handle_lock.as_ref().unwrap().lock().await;
                player.resume(&mut call)?;
                return Ok(());
            }
            "skip" => {
                warn!("Told to skip");
                let player = self.audio_player.lock().await;
                let mut call = player.call_handle_lock.as_ref().unwrap().lock().await;
                player.skip(&mut call)?;
                return Ok(());
            }
            "list" => {
                warn!("Told to print track queue");
                let player = self.audio_player.lock().await;
                player.print_queue(&ctx)?;
                return Ok(());
            }
            "clear" => {
                warn!("Told to clear track queue");
                let player = self.audio_player.lock().await;
                let call = player.call_handle_lock.as_ref().unwrap().lock().await;
                player.clear_queue(&call)?;
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
                else if new_message.content.contains("queue") {
                    let mut player = self.audio_player.lock().await;
                    player.process_enqueue(&ctx, &new_message).await?;
                    return Ok(());
                }
                else if new_message.content.contains("next") {
                    let mut player = self.audio_player.lock().await;
                    player.process_next(&ctx, &new_message).await?;
                    return Ok(());
                }
                else if new_message.content.contains("rm") {
                    let mut player = self.audio_player.lock().await;
                    player.process_rm(&new_message).await?;
                    return Ok(());
                }
                else if new_message.content.contains("goto") {
                    let player = self.audio_player.lock().await;
                    player.process_goto(&new_message).await?;
                    return Ok(());
                }
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
        // Make sure we're listening in our designated channel, and we ignore messages from ourselves
        if (new_message.channel_id == self.audio_text_channel) && !new_message.author.bot {
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