use std::{
    env,
    process::exit,
    sync::{Arc, Mutex},
};

use actix_files::Files;
use actix_web::{
    dev::ServiceRequest, middleware::Logger, web, App, Error, HttpMessage, HttpServer,
};
use actix_web_grants::authorities::AttachAuthorities;
use actix_web_httpauth::{extractors::bearer::BearerAuth, middleware::HttpAuthentication};

#[cfg(not(debug_assertions))]
use actix_web_static_files::ResourceFiles;

use clap::Parser;
use lazy_static::lazy_static;
use path_clean::PathClean;
use simplelog::*;
use sysinfo::{Disks, Networks, System};

pub mod api;
pub mod db;
pub mod utils;

use api::{auth, routes::*};
use db::{db_pool, models::LoginUser};
use utils::{args_parse::Args, control::ProcessControl, db_path, init_config, run_args};

use ffplayout_lib::utils::{init_logging, PlayoutConfig};

#[cfg(not(debug_assertions))]
include!(concat!(env!("OUT_DIR"), "/generated.rs"));

lazy_static! {
    pub static ref ARGS: Args = Args::parse();
    pub static ref DISKS: Arc<Mutex<Disks>> =
        Arc::new(Mutex::new(Disks::new_with_refreshed_list()));
    pub static ref NETWORKS: Arc<Mutex<Networks>> =
        Arc::new(Mutex::new(Networks::new_with_refreshed_list()));
    pub static ref SYS: Arc<Mutex<System>> = Arc::new(Mutex::new(System::new_all()));
}

async fn validator(
    req: ServiceRequest,
    credentials: BearerAuth,
) -> Result<ServiceRequest, (Error, ServiceRequest)> {
    // We just get permissions from JWT
    match auth::decode_jwt(credentials.token()).await {
        Ok(claims) => {
            req.attach(vec![claims.role]);

            req.extensions_mut()
                .insert(LoginUser::new(claims.id, claims.username));

            Ok(req)
        }
        Err(e) => Err((e, req)),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let mut config = PlayoutConfig::new(None);
    config.mail.recipient = String::new();
    config.logging.log_to_file = false;
    config.logging.timestamp = false;

    let logging = init_logging(&config, None, None);
    CombinedLogger::init(logging).unwrap();

    if let Err(c) = run_args().await {
        exit(c);
    }

    let pool = match db_pool().await {
        Ok(p) => p,
        Err(e) => {
            error!("{e}");
            exit(1);
        }
    };

    if let Some(conn) = &ARGS.listen {
        if db_path().is_err() {
            error!("Database is not initialized! Init DB first and add admin user.");
            exit(1);
        }
        init_config(&pool).await;
        let ip_port = conn.split(':').collect::<Vec<&str>>();
        let addr = ip_port[0];
        let port = ip_port[1].parse::<u16>().unwrap();
        let engine_process = web::Data::new(ProcessControl::new());

        info!("running ffplayout API, listen on http://{conn}");

        // no 'allow origin' here, give it to the reverse proxy
        HttpServer::new(move || {
            let auth = HttpAuthentication::bearer(validator);
            let db_pool = web::Data::new(pool.clone());
            // Customize logging format to get IP though proxies.
            let logger = Logger::new("%{r}a \"%r\" %s %b \"%{Referer}i\" \"%{User-Agent}i\" %T")
                .exclude_regex(r"/_nuxt/*");

            let mut web_app = App::new()
                .app_data(db_pool)
                .app_data(engine_process.clone())
                .wrap(logger)
                .service(login)
                .service(
                    web::scope("/api")
                        .wrap(auth)
                        .service(add_user)
                        .service(get_user)
                        .service(get_user_by_name)
                        .service(get_users)
                        .service(remove_user)
                        .service(get_playout_config)
                        .service(update_playout_config)
                        .service(add_preset)
                        .service(get_presets)
                        .service(update_preset)
                        .service(delete_preset)
                        .service(get_channel)
                        .service(get_all_channels)
                        .service(patch_channel)
                        .service(add_channel)
                        .service(remove_channel)
                        .service(update_user)
                        .service(send_text_message)
                        .service(control_playout)
                        .service(media_current)
                        .service(media_next)
                        .service(media_last)
                        .service(process_control)
                        .service(get_playlist)
                        .service(save_playlist)
                        .service(gen_playlist)
                        .service(del_playlist)
                        .service(get_log)
                        .service(file_browser)
                        .service(add_dir)
                        .service(move_rename)
                        .service(remove)
                        .service(save_file)
                        .service(import_playlist)
                        .service(get_program)
                        .service(get_system_stat),
                )
                .service(get_file);

            if let Some(public) = &ARGS.public {
                // When public path is set as argument use this path for serving extra static files,
                // is useful for HLS stream etc.
                let absolute_path = if public.is_absolute() {
                    public.to_path_buf()
                } else {
                    env::current_dir().unwrap_or_default().join(public)
                }
                .clean();

                web_app = web_app.service(Files::new("/", absolute_path));
            } else {
                // When no public path is given as argument, use predefine keywords in path,
                // like /live; /preview; /public, or HLS extensions to recognize file should get from public folder
                web_app = web_app.service(get_public);
            }

            #[cfg(not(debug_assertions))]
            {
                // in release mode embed frontend
                let generated = generate();
                web_app =
                    web_app.service(ResourceFiles::new("/", generated).resolve_not_found_to_root());
            }

            #[cfg(debug_assertions)]
            {
                // in debug mode get frontend from path
                web_app = web_app.service(
                    Files::new("/", "./ffplayout-frontend/.output/public/")
                        .index_file("index.html"),
                );
            }

            web_app
        })
        .bind((addr, port))?
        .run()
        .await
    } else {
        error!("Run ffpapi with listen parameter!");

        Ok(())
    }
}
