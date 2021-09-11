// For sniffer post struct
use crate::reddit::SnifferPost;
use crate::Secrets;

use std::sync::Arc;
use tokio::select;
use tokio::sync::{RwLock, Mutex};
use tokio_util::sync::CancellationToken;

// For Discord
use serenity::{
    model::{id::ChannelId},
    client::{Client, bridge::gateway::ShardManager},
    async_trait,
    prelude::*,
    model::{event::ResumedEvent, gateway::{Ready, Activity}},
    model::channel::{Message, ChannelType, GuildChannel},
};

use songbird::{Songbird, Call};
// Enable songbird register trait for serenity
use songbird::SerenityInit;
use songbird::{ytdl, tracks::create_player};
use songbird::tracks::{TrackHandle};
use songbird::driver::Bitrate;

// For our url regex matching
use regex::Regex;


pub struct DiscordBot {
    serenity_bot: Arc<RwLock<Client>>,
    bot_http: Arc<serenity::http::client::Http>,
    shard_handle: Option<futures_locks::Mutex<tokio::task::JoinHandle<()>>>,
    shard_cancel_token: CancellationToken,
    shard_manager: Arc<Mutex<ShardManager>>,
    chat_channel: ChannelId,
    test_channel: ChannelId,
    archive_channel: ChannelId,
    songbird_instance: Option<Arc<Songbird>>,
    //call_handle_lock: Option<Arc<Mutex<Call>>>,
}

#[derive(Debug, Clone)]
struct PlayerData {
    call_handle_lock: Option<Arc<Mutex<Call>>>,
    //track_handle_lock: Option<Arc<Mutex<TrackHandle>>>,
    track_handle: Option<TrackHandle>,
    songbird: Arc<Songbird>,
}

struct MusicHandler;

impl TypeMapKey for MusicHandler {
    type Value = PlayerData;
}

struct Handler;

#[async_trait]
impl EventHandler for Handler {

    async fn ready(&self, ctx: Context, ready: Ready) {
        warn!("Connected as {}, setting bot to online", ready.user.name);
        set_status(&ctx).await;
    }

    async fn resume(&self, ctx: Context, _: ResumedEvent) {
        warn!("Resumed (reconnected)");
        set_status(&ctx).await;
    }

    async fn message(&self, ctx: Context, new_message: Message) {

        if new_message.channel_id == ChannelId::from(766900346202882058) {
            // TODO: It'd be best not to get a lock on every message coming into this channel
            // but for right now we should be okay
            let mut data = ctx.data.write().await;
            let mut player_data = data.get_mut::<MusicHandler>().expect("Error getting call handler");
            match new_message.content.as_str() {
                "leave" => {
                    warn!("Told to leave");
                    match &player_data.call_handle_lock {
                        Some(h) => {
                            {
                                let mut call_handler = h.lock().await;
                                call_handler.leave().await.expect("Error leaving call");
                            }
                            // Wipe everything
                            player_data.call_handle_lock = None;
                            player_data.track_handle = None;
                        }
                        None => {
                            error!("Told do leave but we weren't here");
                        }
                    }
                }
                "stop" => {
                    warn!("Told to stop");
                    match &player_data.track_handle {
                        Some(h) => {
                            h.stop().expect("Error stopping track");
                            // Wipe our track handle
                            player_data.track_handle = None;
                        }
                        None => {
                            warn!("Told to stop, but not playing anything")
                        }
                    }
                }
                "pause" => {
                    warn!("Told to pause");
                    match &player_data.track_handle {
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
                    match &player_data.track_handle {
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
                        lazy_static! {
                            // Returns the whole string to replace in the first capture, contents of [] in 2nd and () in 3rd
                            static ref RE: Regex = Regex::new(r"https://\S*youtube\S*").unwrap();
                        }
    
                        //error!("{:?}", RE.captures(new_message.content.as_str()));
                        match RE.captures(new_message.content.as_str()) {
                            Some(r) => {
                                warn!("Told to play");
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
                                            let (handler_lock, conn_result) = player_data.songbird.join(current_guild_id, channel_id).await;
                                            conn_result.expect("Error creating songbird call");
                                            player_data.call_handle_lock = Some(handler_lock);
                                        }
                                        else {
                                            error!("we couldn't find our guy");
                                            return;
                                        }
                                        
                                    }
                                }
                                // Create our player
                                let youtube_input = ytdl(url_to_play).await.expect("Error creating ytdl input");
                                let metadata = youtube_input.metadata.clone();
                                warn!("Loaded up youtube: {} - {}", metadata.title.unwrap(), metadata.source_url.unwrap());
                                //error!("YOUTUBE: {:?}", youtube_input);
                                let (audio, track_handle) = create_player(youtube_input);
                                // Closure for lock
                                // Start playing our audio
                                match &player_data.call_handle_lock {
                                    Some(p) => {
                                        let mut handle = p.lock().await;
                                        handle.set_bitrate(bitrate);
                                        handle.play_only(audio);
                                        // Record our track object
                                        player_data.track_handle = Some(track_handle);
                                        warn!("Playing a track!");
                                    }
                                    None => {
                                        panic!("We should absolutely have a handle lock here, something is fucked");
                                    }
                                }  
                            }
                            None => {
                                error!("told to play, but nothing given");
                            }
                        }
                    }
                    else {
                        error!("We got a message here, but it isn't any we are interested in");
                    }
                }
            }
        }
    }
}

// The reset presence and activity action for both ready and result
async fn set_status(ctx: &Context) {
    ctx.reset_presence().await;
    ctx.set_activity(Activity::watching("the sniffer")).await;
}

impl DiscordBot {
    //pub async fn new(token: String, chat_channel: u64, archive_channel: u64, test_channel: u64) -> DiscordBot {
    pub async fn new(secrets: Secrets) -> DiscordBot {
        info!("Created the discord bot");
        // Configure the client with your Discord bot token in the environment.
        let token = secrets.bot_token;

        // Create a songbird instance
        let songbird = Songbird::serenity();
        warn!("Created songbird instance");

        // Create a new instance of the Client, logging in as a bot. This will
        // automatically prepend your bot token with "Bot ", which is a requirement
        // by Discord for bot users.
        let serenity_bot = Client::builder(&token)
            .event_handler(Handler)
            .register_songbird_with(songbird.clone())
            .await
            .expect("Error creating client");
        {
            let mut data = serenity_bot.data.write().await;
            //data.insert::<MusicHandler>(handler_lock.clone())
            data.insert::<MusicHandler>(
                PlayerData {
                    call_handle_lock: None,
                    track_handle: None,
                    songbird: songbird.clone(),
                }
            )
        }
        // Pull our bot user id and initialize songbird with it
        let bot_user_id = serenity_bot.cache_and_http.http.get_current_user().await.expect("couldn't get current user").id;
        songbird.initialise_client_data(1, bot_user_id);
        // Get a shared ref of our http cache so we can use it to send messages in an async fashion
        let http = serenity_bot.cache_and_http.http.clone();
        // And for shard manager too
        let manager_clone = serenity_bot.shard_manager.clone();
        let bot = DiscordBot {
                serenity_bot: Arc::new(RwLock::new(serenity_bot)),
                bot_http: http,
                shard_handle: None,
                shard_cancel_token: CancellationToken::new(),
                shard_manager: manager_clone,
                chat_channel: ChannelId(secrets.main_channel), // main channel
                test_channel: ChannelId(secrets.test_channel),
                archive_channel: ChannelId(secrets.archive_channel), // the archive channel
                songbird_instance: Some(songbird),
                //call_handle_lock: Some(handler_lock),
            };

        return bot;
    }

    pub async fn start_shards(&mut self, num_shards: u64) {
        let bot = self.serenity_bot.clone();
        let cloned_token = self.shard_cancel_token.clone();
        self.shard_handle = Some(futures_locks::Mutex::new(
            tokio::spawn(async move {
                let mut lock = bot.write().await;
                select! {
                    _ = lock.start_shards(num_shards) => {  
                        warn!("Shard threads stopped")
                    }
                    _ = cloned_token.cancelled() => {
                        warn!{"Cancelled our shards"}
                    }
                }
            })
        ));
        warn!("Started shards");
        
    }

    pub async fn print_shard_info(&self) {
        let lock = self.shard_manager.lock().await;
        let shard_runners = lock.runners.lock().await;
        for (id, runner) in shard_runners.iter() {
            warn!(
                "Shard ID {} is {} with a latency of {:?}",
                id, runner.stage, runner.latency,
            );
        }
    }

    pub async fn shutdown(&mut self) {
        //self.stop_audio().await;
        self.stop_shards().await; // we hold a write lock on serenity here, it's its run future
        self.stop_audio().await;
    }

    async fn stop_audio(&self) {  

        //match self.call_handle_lock.as_ref() {
        //let data =  self.songbird_instance.as_ref().unwrap().data.read().await;
        //let mut player_data = data.get::<MusicHandler>().expect("Error getting call handler");
        let serenity_lock = self.serenity_bot.read().await;
        let mut data = serenity_lock.data.write().await;
        let player_data = data.get_mut::<MusicHandler>().expect("Error getting call handler");
        match &player_data.call_handle_lock {
            Some(h) => {
                error!("Leaving call");
                {
                    let mut handler = h.lock().await;
                    handler.leave().await.expect("Error leaving call");
                }
                // This is dumb as hell, but if we don't wait a little bit we'll remove the shards
                // before it has a chance to leave, they should really have a leave_blocking function
                // There's nothing we can poll to check to see if we've fully left either, the
                // associated structs reflect the state immediately

                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            None => {
                error!("No call handle, nothing to hang up on");
            }
        }
    }

    async fn stop_shards(&mut self) {
        // Start the cancel
        self.shard_cancel_token.cancel();
        // Wait on our handle
        match &self.shard_handle{
            Some(x) => {
                let handle_lock = x.lock();
                handle_lock.await;
                warn!("Successfully waited on future");
                //handle_box.await.expect("failed waiting for the sharts to end");
                //*handle_lock.await;
            }
            None => {
                error!("We don't have a shard handle")
            }
        }
    }

    pub async fn post_message(&self, message: SnifferPost) {
        let http = &self.bot_http;
        info!("Trying to send message: {}", message);
        let mut message_text = message.discord_string();

        // Send message to our primary channel
        self.chat_channel.say(&http, message_text.clone()).await.expect("Error sending message to main channel");

        // Send message to our archive channel with url attached
        // Append the post url to this one if we have it
        match message.url { 
            Some(m) => {
                message_text.push_str(format!("\n<{}>", m).as_str());
            }
            None => {}
        }
        self.archive_channel.say(&http, message_text).await.expect("Error sending message to archive");
    }

    /*
    pub async fn songbird_test(&mut self) {
        error!("{:?}", self.songbird_instance);
        // Try to join a server by id
        let guild_id = songbird::id::GuildId::from(713563112728690689);
        let channel_id = songbird::id::ChannelId::from(745482345099952129);
        let user_id = songbird::id::UserId::from(842586247720861737);

        self.songbird_instance.as_ref().unwrap().initialise_client_data(1, user_id);
        error!("SONGBIRD: {:?}", self.songbird_instance.as_ref().unwrap());
        // try to join the call
        let (handler_lock, conn_result) = self.songbird_instance.as_ref().unwrap().join(guild_id, channel_id).await;

        // Put the lock in our main struct
        self.call_handle_lock = Some(handler_lock.clone());
        error!("TEST: {:?}", self.call_handle_lock);
        
        match conn_result {
            Ok(_) => {
                error!("We connected to the voice channel!");

                let youtube_input = ytdl("https://www.youtube.com/watch?v=zFzCHsIXDhE").await.expect("Error creating ytdl input");
                error!("YOUTUBE: {:?}", youtube_input);
                let (mut audio, audio_handle) = create_player(youtube_input);
                // Closure for lock
                {
                    let mut handler = handler_lock.lock().await;
                    audio.set_volume(0.2);
                    handler.play_only(audio);
                }
                
                //std::thread::sleep_ms(10000);
                //audio_handle.stop().expect("Error stopping audio");
                //std::thread::sleep_ms(5000);

                // Another closure for lock
                //{
                //    let mut handler = handler_lock.lock().await;
                //    handler.leave().await.expect("Error leaving call");
                //}

            }
            Err(_) => {
                error!("Error connecting to the voice channel!");
            }
        }
        
    }
    */

    #[allow(dead_code)]
    pub async fn post_debug_string(&self, message: String) {
        let http = &self.bot_http;
        warn!("Trying to send debug message");
        self.test_channel.say(&http, message.clone()).await.expect("Error sending test message");
    }

}

impl Clone for DiscordBot {
    fn clone(&self) -> Self {
        DiscordBot {
            serenity_bot: self.serenity_bot.clone(),
            bot_http: self.bot_http.clone(),
            shard_handle: {
                match &self.shard_handle {
                    Some(h) => Some(h.clone()),
                    None => None,
                }
            },
            shard_cancel_token: self.shard_cancel_token.clone(),
            shard_manager: self.shard_manager.clone(),
            chat_channel: self.chat_channel.clone(),
            test_channel: self.test_channel.clone(),
            archive_channel: self.archive_channel.clone(),
            songbird_instance: self.songbird_instance.clone(),
            //call_handle_lock: self.call_handle_lock.clone(),
        }
    }
}

