use aimdb_core::AimDB;

#[tokio::main]
async fn main() {
    let db = AimDB::new();
    println!("AimDB SingleLatest async example started: {:?}", db);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_db_creation() {
        let db = AimDB::new();
        assert!(format!("{:?}", db).contains("AimDB"));
    }

    #[tokio::test]
    async fn test_db_multiple_instances() {
        let db1 = AimDB::new();
        let db2 = AimDB::new();
        assert!(format!("{:?}", db1).contains("AimDB"));
        assert!(format!("{:?}", db2).contains("AimDB"));
    }
}
