// Formatting
use std::fmt;
// For reddit functions
use roux::User;
use roux::util::RouxError;


#[derive(Debug, Clone)]
pub struct SnifferPost {
    pub title: String,
    pub body: Option<String>,
    pub subreddit: String,
    pub url: Option<String>,
    pub id: String,
    pub timestamp: u64,
}

// TODO: Make sense of the timestamps, so that if the post is deleted we can post how long it took for luls
impl SnifferPost {
    pub fn from_roux(roux: roux::subreddit::responses::SubmissionsData) -> SnifferPost {
        debug!("creating a new sniffer post object");
        SnifferPost {
            title: roux.title,
            body: {
                if roux.selftext.is_empty() {
                    None
                }
                else {
                    Some(roux.selftext)
                }
            },
            subreddit: roux.subreddit,
            url: roux.url,
            id: roux.id,
            timestamp: roux.created as u64,
        }
    }
    pub fn discord_string(&self) -> String {
        // If we have body text, use it
        match &self.body {
            Some(b) => format!("{}\n```\n{}\n```> /r/{}", self.title, b, self.subreddit),
            None => format!("{}\n> /r/{}", self.title, self.subreddit)
        }

    }
}


pub struct RedditScraper {
    the_sniffer: roux::User,
    last_post_timestamp: u64,
    post_cache: Vec<SnifferPost>,
}

impl PartialEq for SnifferPost {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl fmt::Display for SnifferPost {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}, {:?}, {}, {:?}", self.title, self.body, self.subreddit, self.url)    
    }
}

impl RedditScraper {

    pub fn new(sniffer: String) -> RedditScraper {
        debug!("Created the reddit scraper");
        let scraper = RedditScraper {
            the_sniffer: User::new(sniffer.as_str()),
            last_post_timestamp: 0,
            post_cache: Vec::new()
        };

        scraper.init()
    }

    fn init(mut self) -> RedditScraper {
        // Get from reddit api
        let mut reddit_posts = self.pull_posts().expect("Error getting initial posts");

        // Add our pulled posts to our cache
        self.post_cache.append(&mut reddit_posts);

        // update our most recent timestamp
        self.last_post_timestamp = self.post_cache.last().unwrap().timestamp;

        warn!("Pulled {} intial posts", self.post_cache.len());

        return self;
    }

    fn pull_posts(&self) -> Result<Vec<SnifferPost>, RouxError> {
        // Get from reddit api

        // dumb shit to run async in a sync function
        let reddit_posts = tokio::task::block_in_place(move || {
            tokio::runtime::Handle::current().block_on(async move {
                self.the_sniffer.submitted().await
            })
        });
        match reddit_posts {
            Ok(submissions_data) => {
                let mut new_posts = Vec::<SnifferPost>::new();
                for p in submissions_data.data.children {
                    new_posts.push(SnifferPost::from_roux(p.data));
                }
                // Always sort our posts oldest->newest bc reddit just gives them in random order
                new_posts.sort_by(|a, b| a.timestamp.partial_cmp(&b.timestamp).unwrap());
                return Ok(new_posts);
            }
            Err(error) => {
                    error!("Encountered an error grabbing reddit posts\n{}", error);
                    return Err(error)
                },
        };
    }
    
    pub fn update(&mut self) -> Result<Option<Vec<SnifferPost>>, RouxError> {

        debug!("Updating reddit posts");

        // Strip the async requirement out of this function
        //let posts_result = tokio::task::block_in_place(move || {

        let posts_result = self.pull_posts();

        //let fresh_posts = match self.pull_posts().await {
        let fresh_posts = match posts_result {
            Ok(d) => d,
            Err(e) => return Err(e),
        };

        // Our vec of potential new posts
        let mut new_posts = Vec::<SnifferPost>::new();

        // Check our new posts with our cache to see if any exist
        for p in fresh_posts {
            // we only need to check the new post timestamps against the last recorded one
            if p.timestamp > self.last_post_timestamp {
                // Double-check to make sure that reddit didn't decide to "update" the timestamp on an older post
                match self.post_cache.iter_mut().find(|x| *x.id == p.id) {
                    Some(x) => { 
                        error!("Reddit gave us an incorrectly modified timestamp on existing post {}", x.id);
                        // update the post with the new timestamp, thanks reddit
                        error!("Updating {} timestamp to {} from {}", x.id, x.timestamp, p.timestamp);
                        x.timestamp = p.timestamp;
                        // Update our last_post_timestamp after correction
                        self.last_post_timestamp = p.timestamp;
                    }
                    None => {
                        warn!("New sniffer post {}", p);
                        // record our new posts in the cache
                        self.post_cache.push(p.clone());
                        warn!("Cached the new post");
                        // Add our new posts
                        new_posts.push(p);
                        // Update the most recent timestamp 
                        self.last_post_timestamp = new_posts.last().unwrap().timestamp;
                    },
                }    
            } // If there's no new post detected, we don't put any in our vec
        }

        if !new_posts.is_empty() {
            // record our new posts in the cache
            return Ok(Some(new_posts));
        }
        return Ok(None);
    }

}
