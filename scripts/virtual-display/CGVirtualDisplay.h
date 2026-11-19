// CoreGraphics' virtual display classes: private, no public header. The interface below is
// the one Chromium declares for its own display tests, in
// ui/display/mac/test/virtual_display_mac_util.mm
// (https://source.chromium.org/chromium/chromium/src/+/main:ui/display/mac/test/virtual_display_mac_util.mm),
// also used by open-source display tools such as FluffyDisplay and DeskPad. Only what
// scripts/virtual-display/main.swift calls is declared here.
#import <CoreGraphics/CoreGraphics.h>
#import <Foundation/Foundation.h>

@class CGVirtualDisplay;

@interface CGVirtualDisplayDescriptor : NSObject
@property(retain, nonatomic) dispatch_queue_t queue;
@property(retain, nonatomic) NSString *name;
@property(nonatomic) unsigned int maxPixelsHigh;
@property(nonatomic) unsigned int maxPixelsWide;
@property(nonatomic) CGSize sizeInMillimeters;
@property(nonatomic) unsigned int serialNum;
@property(nonatomic) unsigned int productID;
@property(nonatomic) unsigned int vendorID;
@property(copy, nonatomic) void (^terminationHandler)(id, CGVirtualDisplay *);
@end

@interface CGVirtualDisplayMode : NSObject
@property(readonly, nonatomic) double refreshRate;
@property(readonly, nonatomic) unsigned int height;
@property(readonly, nonatomic) unsigned int width;
- (instancetype)initWithWidth:(unsigned int)width height:(unsigned int)height refreshRate:(double)refreshRate;
@end

@interface CGVirtualDisplaySettings : NSObject
@property(retain, nonatomic) NSArray<CGVirtualDisplayMode *> *modes;
@property(nonatomic) unsigned int hiDPI;
@end

@interface CGVirtualDisplay : NSObject
@property(readonly, nonatomic) unsigned int displayID;
- (instancetype)initWithDescriptor:(CGVirtualDisplayDescriptor *)descriptor;
- (BOOL)applySettings:(CGVirtualDisplaySettings *)settings;
@end
