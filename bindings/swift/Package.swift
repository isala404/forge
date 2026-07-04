// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "ForgeLib",
    products: [
        .library(name: "ForgeLib", targets: ["ForgeLib"])
    ],
    targets: [
        .target(name: "ForgeLib")
    ]
)
