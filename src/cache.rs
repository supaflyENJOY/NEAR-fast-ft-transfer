use moka::future::Cache;
use near_primitives::types::AccountId;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum CacheKey {
    AccountExists(AccountId),
    StorageDeposit {
        account_id: AccountId,
        contract: AccountId,
    },
}

pub struct AccountValidationCache {
    cache: Cache<CacheKey, bool>,
}

impl AccountValidationCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            cache: Cache::builder().time_to_live(ttl).build(),
        }
    }

    pub async fn get_account_exists(&self, account_id: &AccountId) -> Option<bool> {
        self.cache
            .get(&CacheKey::AccountExists(account_id.clone()))
            .await
    }

    pub async fn set_account_exists(&self, account_id: &AccountId, exists: bool) {
        self.cache
            .insert(CacheKey::AccountExists(account_id.clone()), exists)
            .await;
    }

    pub async fn get_storage_deposit(
        &self,
        account_id: &AccountId,
        contract: &AccountId,
    ) -> Option<bool> {
        self.cache
            .get(&CacheKey::StorageDeposit {
                account_id: account_id.clone(),
                contract: contract.clone(),
            })
            .await
    }

    pub async fn set_storage_deposit(
        &self,
        account_id: &AccountId,
        contract: &AccountId,
        has_deposit: bool,
    ) {
        self.cache
            .insert(
                CacheKey::StorageDeposit {
                    account_id: account_id.clone(),
                    contract: contract.clone(),
                },
                has_deposit,
            )
            .await;
    }

    pub async fn invalidate_account(&self, account_id: &AccountId) {
        self.cache
            .invalidate(&CacheKey::AccountExists(account_id.clone()))
            .await;
    }

    pub fn entry_count(&self) -> u64 {
        self.cache.entry_count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_account_exists_cache() {
        let cache = AccountValidationCache::new(Duration::from_secs(60));
        let account: AccountId = "test.near".parse().unwrap();

        assert_eq!(cache.get_account_exists(&account).await, None);

        cache.set_account_exists(&account, true).await;
        assert_eq!(cache.get_account_exists(&account).await, Some(true));

        cache.set_account_exists(&account, false).await;
        assert_eq!(cache.get_account_exists(&account).await, Some(false));
    }

    #[tokio::test]
    async fn test_storage_deposit_cache() {
        let cache = AccountValidationCache::new(Duration::from_secs(60));
        let account: AccountId = "test.near".parse().unwrap();
        let contract: AccountId = "ft.near".parse().unwrap();

        assert_eq!(cache.get_storage_deposit(&account, &contract).await, None);

        cache.set_storage_deposit(&account, &contract, true).await;
        assert_eq!(
            cache.get_storage_deposit(&account, &contract).await,
            Some(true)
        );

        cache.set_storage_deposit(&account, &contract, false).await;
        assert_eq!(
            cache.get_storage_deposit(&account, &contract).await,
            Some(false)
        );
    }

    #[tokio::test]
    async fn test_cache_ttl() {
        let cache = AccountValidationCache::new(Duration::from_millis(100));
        let account: AccountId = "test.near".parse().unwrap();

        cache.set_account_exists(&account, true).await;
        assert_eq!(cache.get_account_exists(&account).await, Some(true));

        tokio::time::sleep(Duration::from_millis(150)).await;

        assert_eq!(cache.get_account_exists(&account).await, None);
    }

    #[tokio::test]
    async fn test_multiple_accounts() {
        let cache = AccountValidationCache::new(Duration::from_secs(60));
        let account1: AccountId = "test1.near".parse().unwrap();
        let account2: AccountId = "test2.near".parse().unwrap();

        cache.set_account_exists(&account1, true).await;
        cache.set_account_exists(&account2, false).await;

        assert_eq!(cache.get_account_exists(&account1).await, Some(true));
        assert_eq!(cache.get_account_exists(&account2).await, Some(false));
    }
}
