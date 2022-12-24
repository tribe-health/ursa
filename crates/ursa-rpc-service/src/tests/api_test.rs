#[cfg(test)]
mod tests {
    use crate::api::{NetworkInterface, NodeNetworkInterface};
    use crate::tests::{init, setup_logger};
    use async_fs::{remove_file, File};
    use futures::io::BufReader;
    use fvm_ipld_car::load_car;
    use std::path::Path;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_put_and_get() -> anyhow::Result<()> {
        setup_logger();
        let (ursa_service, mut provider_engine, store) = init()?;

        let interface = Arc::new(NodeNetworkInterface {
            store: Arc::clone(&store),
            network_send: ursa_service.command_sender(),
            provider_send: provider_engine.command_sender(),
        });

        // the test case does not start the provider engine, so the best way
        // for put_file to not call provider engine is to close the channel
        provider_engine.command_receiever().close();

        let put_file = interface
            .put_file("../../test_files/test.car".to_string())
            .await?;
        let root_cid = put_file[0];

        interface
            .get_file("../../test_files".to_string(), root_cid)
            .await?;

        let path = format!("../../test_files/{}.car", root_cid.to_string());
        let path = Path::new(&path);
        let file = File::open(path).await?;
        let reader = BufReader::new(file);
        let cids = load_car(store.blockstore(), reader).await?;

        assert_eq!(cids[0], root_cid);
        remove_file(path).await?;

        Ok(())
    }
}