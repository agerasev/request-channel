use crate::channel;
use futures::{executor::block_on, future::join_all, join};

#[test]
fn single() {
    let (req, mut res) = channel::<i32, i32>();
    block_on(async {
        join!(
            async move {
                let r = req.request(1).unwrap();
                assert_eq!(r.get_response().await.unwrap(), 2);
            },
            async move {
                let (x, r) = res.next().await.unwrap();
                r.respond(x * 2);
            }
        );
    });
}

#[test]
fn multiple() {
    let (req, mut res) = channel::<i32, i32>();
    block_on(async {
        join!(
            async move {
                let xs = 0..32;
                let rs = xs.clone().into_iter().map(|i| req.request(i).unwrap());

                assert!(join_all(rs.into_iter().map(|r| r.get_response()))
                    .await
                    .into_iter()
                    .map(|y| y.unwrap())
                    .eq(xs.into_iter().map(|x| x * 2)));
            },
            async move {
                while let Some((x, r)) = res.next().await {
                    r.respond(x * 2);
                }
            }
        );
    });
}

#[test]
fn no_resonse() {
    let (req, mut res) = channel::<i32, i32>();
    block_on(async {
        join!(
            async move {
                let r = req.request(1).unwrap();
                assert_eq!(r.get_response().await, None);
            },
            async move {
                let _ = res.next().await.unwrap();
            }
        );
    });
}

#[test]
fn request_and_drop() {
    let (req, mut res) = channel::<i32, i32>();
    block_on(async {
        join!(
            async move {
                let _ = req.request(1).unwrap();
            },
            async move {
                let (x, r) = res.next().await.unwrap();
                r.respond(2 * x);
            }
        );
    });
}
